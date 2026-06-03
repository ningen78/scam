//! Core domain types shared across parsing, planning, and execution.

use std::{path::PathBuf, thread, time::Duration};

use crate::metadata::MetadataSnapshot;

/// Default hashing buffer size in bytes.
///
/// A 4 MiB buffer keeps checksum generation fast on modern SSDs without using a
/// large amount of memory per file.
pub const DEFAULT_HASH_BUFFER_SIZE: usize = 4 * 1024 * 1024;

/// Default copy buffer size in bytes.
///
/// Manual copy and resume operations use a slightly larger buffer to reduce the
/// number of syscalls during large sequential transfers.
pub const DEFAULT_COPY_BUFFER_SIZE: usize = 8 * 1024 * 1024;

/// How often the optional progress reporter emits a status line.
pub const DEFAULT_PROGRESS_UPDATE_INTERVAL: Duration = Duration::from_millis(250);

/// The high-level operation requested by the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    /// Copy the source and keep the original in place.
    Copy,
    /// Copy the source, verify it, and then remove the original.
    Move,
}

impl Operation {
    /// Returns `true` when the operation removes the source after verification.
    pub const fn is_move(self) -> bool {
        matches!(self, Self::Move)
    }

    /// Human-friendly verb for status output.
    pub const fn present_tense(self) -> &'static str {
        match self {
            Self::Copy => "copy",
            Self::Move => "move",
        }
    }

    /// Human-friendly past tense for status output.
    pub const fn past_tense(self) -> &'static str {
        match self {
            Self::Copy => "Copied",
            Self::Move => "Moved",
        }
    }
}

/// Runtime execution settings that affect performance, output, and retry behavior.
#[derive(Debug, Clone)]
pub struct ExecutionSettings {
    /// Buffer size used while hashing files.
    pub hash_buffer_size: usize,
    /// Buffer size used while manually copying files.
    pub copy_buffer_size: usize,
    /// Maximum number of attempts per file.
    ///
    /// `None` means retry forever until the file verifies successfully.
    pub max_retries: Option<usize>,
    /// Delay inserted after a failed attempt and before retrying.
    pub retry_delay: Duration,
    /// Whether to print a success summary to stderr.
    pub verbose: bool,
    /// Whether to print live progress updates to stderr.
    pub progress: bool,
    /// Number of worker threads used for directory file transfers.
    pub jobs: usize,
}

impl Default for ExecutionSettings {
    fn default() -> Self {
        Self {
            hash_buffer_size: DEFAULT_HASH_BUFFER_SIZE,
            copy_buffer_size: DEFAULT_COPY_BUFFER_SIZE,
            max_retries: None,
            retry_delay: Duration::ZERO,
            verbose: false,
            progress: false,
            jobs: default_worker_threads(),
        }
    }
}

/// A single directory that belongs to a recursive transfer.
#[derive(Debug, Clone)]
pub struct DirectoryTransfer {
    /// Absolute or user-provided source path.
    pub source: PathBuf,
    /// Final destination path for the directory.
    pub destination: PathBuf,
    /// Preserved metadata captured from the source directory before transfer.
    pub metadata: MetadataSnapshot,
}

/// A single file that must be copied and verified.
#[derive(Debug, Clone)]
pub struct FileTransfer {
    /// Source file to copy.
    pub source: PathBuf,
    /// Destination file path.
    pub destination: PathBuf,
    /// Logical size of the source file in bytes.
    pub size_bytes: u64,
    /// Preserved metadata captured from the source file before transfer.
    pub metadata: MetadataSnapshot,
}

/// Fully resolved work plan derived from command-line arguments.
#[derive(Debug, Clone)]
pub struct TransferPlan {
    /// Whether the user requested a copy or a move.
    pub operation: Operation,
    /// Root path provided as the source.
    pub source_root: PathBuf,
    /// Final root destination after cp/mv-style path resolution.
    pub destination_root: PathBuf,
    /// Every directory that must exist at the destination.
    pub directories: Vec<DirectoryTransfer>,
    /// Every file that must be copied and verified.
    pub files: Vec<FileTransfer>,
}

impl TransferPlan {
    /// Returns `true` when the source is a directory transfer.
    pub fn is_directory_transfer(&self) -> bool {
        !self.directories.is_empty()
    }

    /// Sum the logical payload size of every file in the plan.
    pub fn total_bytes(&self) -> u64 {
        self.files
            .iter()
            .fold(0_u64, |total, file| total.saturating_add(file.size_bytes))
    }
}

/// Aggregate transfer statistics returned after a successful run.
#[derive(Debug, Clone, Default)]
pub struct RunSummary {
    /// Number of directories involved in the plan.
    pub directories: usize,
    /// Number of files that were copied and verified.
    pub files: usize,
    /// Total logical payload bytes completed.
    pub bytes_copied: u64,
    /// Number of files that were resumed from a matching destination prefix.
    pub resumed_files: usize,
    /// Number of bytes that were reused from resumable destination files.
    pub resumed_bytes: u64,
    /// Total number of retry attempts across every file.
    pub retries: usize,
    /// Total wall-clock runtime.
    pub elapsed: Duration,
}

/// Determine the default worker-thread count used for directory transfers.
pub fn default_worker_threads() -> usize {
    thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1)
}
