//! Per-file verified copy and move logic.
//!
//! This module owns the retry loop, resumable partial-copy handling, and the
//! source-versus-destination checksum verification that makes `scam` different
//! from a regular `cp` / `mv` wrapper.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, ErrorKind, Read, Seek, SeekFrom, Write},
    path::{Path, absolute},
    sync::Arc,
    thread,
};

use blake3::Hash;

use crate::{
    checksum::{checksum_file, checksum_hex, file_matches},
    error::{Result, ScamError},
    model::{ExecutionSettings, FileTransfer},
    progress::{FileProgress, ProgressReporter},
};

/// Aggregate result for one successfully processed file.
#[derive(Debug, Clone, Copy, Default)]
pub struct FileSummary {
    /// Logical payload bytes represented by the file.
    pub bytes_copied: u64,
    /// Number of retries needed before the file succeeded.
    pub retries: usize,
    /// Number of bytes that were reused from a matching existing destination.
    pub resumed_bytes: u64,
}

enum AttemptFailure {
    Retry(String),
    Fatal(ScamError),
}

struct AttemptSuccess {
    resumed_bytes: u64,
}

enum DestinationState {
    StartFromZero,
    ResumeFrom { offset: u64 },
    AlreadyComplete,
}

/// Copy one file, verify it, preserve metadata, and optionally delete the
/// original after success.
pub fn transfer_file_with_verification(
    file: &FileTransfer,
    remove_source_after_success: bool,
    settings: &ExecutionSettings,
    progress: Option<&Arc<ProgressReporter>>,
) -> Result<FileSummary> {
    let expected_checksum = checksum_file(&file.source, settings.hash_buffer_size)?;
    let mut attempts = 0usize;
    let mut file_progress = progress.map(|reporter| reporter.begin_file(file.size_bytes));

    loop {
        attempts += 1;

        match attempt_copy_and_verify(file, &expected_checksum, settings, file_progress.as_mut()) {
            Ok(success) => {
                file.metadata.apply_to_path(&file.destination)?;

                if remove_source_after_success {
                    verify_source_before_deleting(
                        &file.source,
                        &expected_checksum,
                        settings.hash_buffer_size,
                    )?;
                    fs::remove_file(&file.source).map_err(|source| {
                        ScamError::io(
                            "removing source file after verified move",
                            &file.source,
                            source,
                        )
                    })?;
                }

                if let Some(progress) = file_progress.as_mut() {
                    progress.complete();
                }

                return Ok(FileSummary {
                    bytes_copied: file.size_bytes,
                    retries: attempts.saturating_sub(1),
                    resumed_bytes: success.resumed_bytes,
                });
            }
            Err(AttemptFailure::Retry(reason)) => {
                if let Some(progress) = file_progress.as_mut() {
                    progress.reset_attempt();
                }

                if let Some(limit) = settings.max_retries
                    && attempts >= limit
                {
                    return Err(ScamError::RetryLimitExceeded {
                        path: file.source.clone(),
                        attempts,
                    });
                }

                if let Some(reporter) = progress {
                    reporter.note_retry(&file.source, attempts, &reason);
                } else {
                    eprintln!("Retry {attempts} for `{}`: {reason}", file.source.display());
                }

                if !settings.retry_delay.is_zero() {
                    thread::sleep(settings.retry_delay);
                }
            }
            Err(AttemptFailure::Fatal(error)) => return Err(error),
        }
    }
}

fn attempt_copy_and_verify(
    file: &FileTransfer,
    expected_checksum: &Hash,
    settings: &ExecutionSettings,
    mut progress: Option<&mut FileProgress>,
) -> std::result::Result<AttemptSuccess, AttemptFailure> {
    ensure_destination_parent_exists(&file.destination).map_err(AttemptFailure::Fatal)?;

    let destination_state =
        prepare_destination(file, expected_checksum, settings, progress.as_deref_mut())?;
    let resumed_bytes = match destination_state {
        DestinationState::StartFromZero => 0,
        DestinationState::ResumeFrom { offset } => offset,
        DestinationState::AlreadyComplete => {
            return Ok(AttemptSuccess {
                resumed_bytes: file.size_bytes,
            });
        }
    };

    let manual_copy = settings.progress || resumed_bytes > 0;
    if manual_copy {
        copy_file_manually(file, resumed_bytes, settings, progress)?;
    } else if let Err(source) = fs::copy(&file.source, &file.destination) {
        return Err(copy_fast_path_failure(&file.source, source));
    }

    let actual_checksum = checksum_file(&file.destination, settings.hash_buffer_size)
        .map_err(|error| AttemptFailure::Retry(format!("destination checksum failed: {error}")))?;

    if actual_checksum == *expected_checksum {
        return Ok(AttemptSuccess { resumed_bytes });
    }

    match source_matches_expected(&file.source, expected_checksum, settings.hash_buffer_size) {
        Ok(true) => {
            let _ = remove_file_if_exists(&file.destination);
            Err(AttemptFailure::Retry(format!(
                "checksum mismatch (source {}, destination {})",
                checksum_hex(expected_checksum),
                checksum_hex(&actual_checksum)
            )))
        }
        Ok(false) => Err(AttemptFailure::Fatal(ScamError::SourceChanged(
            file.source.clone(),
        ))),
        Err(error) => Err(AttemptFailure::Fatal(error)),
    }
}

fn prepare_destination(
    file: &FileTransfer,
    expected_checksum: &Hash,
    settings: &ExecutionSettings,
    mut progress: Option<&mut FileProgress>,
) -> std::result::Result<DestinationState, AttemptFailure> {
    let metadata = match fs::symlink_metadata(&file.destination) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Ok(DestinationState::StartFromZero);
        }
        Err(source) => {
            return Err(AttemptFailure::Retry(format!(
                "reading destination metadata failed: {source}"
            )));
        }
    };

    if metadata.file_type().is_symlink() {
        return Err(AttemptFailure::Fatal(ScamError::UnsupportedSymlink(
            file.destination.clone(),
        )));
    }

    if metadata.is_dir() {
        return Err(AttemptFailure::Fatal(ScamError::DestinationIsDirectory(
            file.destination.clone(),
        )));
    }

    let same_path = same_file::is_same_file(&file.source, &file.destination).map_err(|source| {
        AttemptFailure::Fatal(ScamError::io(
            "comparing source and destination",
            &file.destination,
            source,
        ))
    })?;
    if same_path {
        let resolved = absolute(&file.destination).map_err(|source| {
            AttemptFailure::Fatal(ScamError::io(
                "resolving destination path",
                &file.destination,
                source,
            ))
        })?;
        return Err(AttemptFailure::Fatal(ScamError::SamePath(resolved)));
    }

    let destination_len = metadata.len();
    if destination_len == file.size_bytes {
        let destination_checksum = checksum_file(&file.destination, settings.hash_buffer_size)
            .map_err(|error| {
                AttemptFailure::Retry(format!("destination checksum failed: {error}"))
            })?;
        if destination_checksum == *expected_checksum {
            if let Some(progress) = progress.as_deref_mut() {
                progress.account_resume(file.size_bytes);
            }
            return Ok(DestinationState::AlreadyComplete);
        }

        restart_destination_file(&file.destination)?;
        return Ok(DestinationState::StartFromZero);
    }

    if destination_len > file.size_bytes {
        restart_destination_file(&file.destination)?;
        return Ok(DestinationState::StartFromZero);
    }

    if destination_len == 0 {
        return Ok(DestinationState::StartFromZero);
    }

    match destination_matches_source_prefix(
        &file.source,
        &file.destination,
        destination_len,
        settings.copy_buffer_size,
    )? {
        true => {
            if let Some(progress) = progress {
                progress.account_resume(destination_len);
            }
            Ok(DestinationState::ResumeFrom {
                offset: destination_len,
            })
        }
        false => {
            restart_destination_file(&file.destination)?;
            Ok(DestinationState::StartFromZero)
        }
    }
}

fn copy_file_manually(
    file: &FileTransfer,
    resume_from: u64,
    settings: &ExecutionSettings,
    mut progress: Option<&mut FileProgress>,
) -> std::result::Result<(), AttemptFailure> {
    let mut source_file = open_source_for_copy(&file.source)?;
    if resume_from > 0 {
        source_file
            .seek(SeekFrom::Start(resume_from))
            .map_err(|source| copy_read_failure(&file.source, source))?;
    }

    let mut destination_file = open_destination_for_write(&file.destination, resume_from)?;
    let mut buffer = vec![0_u8; settings.copy_buffer_size.max(8 * 1024)];

    loop {
        let bytes_read = source_file
            .read(&mut buffer)
            .map_err(|source| copy_read_failure(&file.source, source))?;
        if bytes_read == 0 {
            break;
        }

        destination_file
            .write_all(&buffer[..bytes_read])
            .map_err(|source| {
                AttemptFailure::Retry(format!("writing destination failed: {source}"))
            })?;

        if let Some(progress) = progress.as_deref_mut() {
            progress.account_copy(bytes_read as u64);
        }
    }

    destination_file
        .flush()
        .map_err(|source| AttemptFailure::Retry(format!("flushing destination failed: {source}")))
}

fn destination_matches_source_prefix(
    source: &Path,
    destination: &Path,
    prefix_length: u64,
    buffer_size: usize,
) -> std::result::Result<bool, AttemptFailure> {
    let mut source_file = open_source_for_copy(source)?;
    let mut destination_file = File::open(destination).map_err(|source| {
        AttemptFailure::Retry(format!("opening existing destination failed: {source}"))
    })?;
    let mut source_buffer = vec![0_u8; buffer_size.max(8 * 1024)];
    let mut destination_buffer = vec![0_u8; buffer_size.max(8 * 1024)];
    let mut remaining = prefix_length;

    while remaining > 0 {
        let chunk_size = remaining.min(source_buffer.len() as u64) as usize;

        source_file
            .read_exact(&mut source_buffer[..chunk_size])
            .map_err(|source_error| copy_read_failure(source, source_error))?;
        destination_file
            .read_exact(&mut destination_buffer[..chunk_size])
            .map_err(|source| {
                AttemptFailure::Retry(format!("reading existing destination failed: {source}"))
            })?;

        if source_buffer[..chunk_size] != destination_buffer[..chunk_size] {
            return Ok(false);
        }

        remaining -= chunk_size as u64;
    }

    Ok(true)
}

fn open_source_for_copy(path: &Path) -> std::result::Result<File, AttemptFailure> {
    File::open(path).map_err(|source| source_open_failure(path, source))
}

fn open_destination_for_write(
    path: &Path,
    resume_from: u64,
) -> std::result::Result<File, AttemptFailure> {
    let mut options = OpenOptions::new();
    options.create(true).write(true);
    if resume_from == 0 {
        options.truncate(true);
    }

    let mut file = options.open(path).map_err(|source| {
        AttemptFailure::Retry(format!("opening destination for writing failed: {source}"))
    })?;

    if resume_from > 0 {
        file.seek(SeekFrom::Start(resume_from)).map_err(|source| {
            AttemptFailure::Retry(format!("seeking destination failed: {source}"))
        })?;
    }

    Ok(file)
}

fn ensure_destination_parent_exists(path: &Path) -> Result<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };

    if parent.as_os_str().is_empty() {
        return Ok(());
    }

    fs::create_dir_all(parent)
        .map_err(|source| ScamError::io("creating destination parent directories", parent, source))
}

fn restart_destination_file(path: &Path) -> std::result::Result<(), AttemptFailure> {
    remove_file_if_exists(path).map_err(|source| {
        AttemptFailure::Retry(format!("removing stale destination failed: {source}"))
    })
}

fn remove_file_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn copy_fast_path_failure(path: &Path, source: io::Error) -> AttemptFailure {
    if source.kind() == ErrorKind::NotFound && !path.exists() {
        AttemptFailure::Fatal(ScamError::SourceChanged(path.to_path_buf()))
    } else {
        AttemptFailure::Retry(format!("copy failed: {source}"))
    }
}

fn source_open_failure(path: &Path, source: io::Error) -> AttemptFailure {
    if source.kind() == ErrorKind::NotFound {
        AttemptFailure::Fatal(ScamError::SourceChanged(path.to_path_buf()))
    } else {
        AttemptFailure::Retry(format!("opening source failed: {source}"))
    }
}

fn copy_read_failure(path: &Path, source: io::Error) -> AttemptFailure {
    if matches!(
        source.kind(),
        ErrorKind::NotFound | ErrorKind::UnexpectedEof
    ) {
        AttemptFailure::Fatal(ScamError::SourceChanged(path.to_path_buf()))
    } else {
        AttemptFailure::Retry(format!("reading source failed: {source}"))
    }
}

fn source_matches_expected(path: &Path, expected: &Hash, buffer_size: usize) -> Result<bool> {
    match file_matches(path, expected, buffer_size) {
        Ok(matches) => Ok(matches),
        Err(ScamError::Io { source, .. }) if source.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn verify_source_before_deleting(path: &Path, expected: &Hash, buffer_size: usize) -> Result<()> {
    if source_matches_expected(path, expected, buffer_size)? {
        Ok(())
    } else {
        Err(ScamError::SourceChanged(path.to_path_buf()))
    }
}
