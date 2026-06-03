//! Transfer planning and cp/mv-style path resolution.

use std::{
    fs,
    path::{Path, PathBuf, absolute},
};

use walkdir::WalkDir;

use crate::{
    error::{Result, ScamError},
    metadata::MetadataSnapshot,
    model::{DirectoryTransfer, FileTransfer, Operation, TransferPlan},
};

/// Resolve the user's source/destination arguments into a concrete transfer plan.
pub fn build_transfer_plan(
    operation: Operation,
    source_root: PathBuf,
    destination: PathBuf,
) -> Result<TransferPlan> {
    let source_metadata = fs::symlink_metadata(&source_root)
        .map_err(|source| ScamError::io("reading source metadata", &source_root, source))?;

    if source_metadata.file_type().is_symlink() {
        return Err(ScamError::UnsupportedSymlink(source_root));
    }

    if source_metadata.is_file() {
        build_file_plan(operation, source_root, destination)
    } else if source_metadata.is_dir() {
        build_directory_plan(operation, source_root, destination)
    } else {
        Err(ScamError::UnsupportedSourceType(source_root))
    }
}

fn build_file_plan(
    operation: Operation,
    source_root: PathBuf,
    destination: PathBuf,
) -> Result<TransferPlan> {
    let destination_root = resolve_file_destination(&source_root, &destination)?;
    reject_same_path(&source_root, &destination_root)?;
    let source_metadata = fs::metadata(&source_root)
        .map_err(|source| ScamError::io("reading source metadata", &source_root, source))?;

    Ok(TransferPlan {
        operation,
        source_root: source_root.clone(),
        destination_root: destination_root.clone(),
        directories: Vec::new(),
        files: vec![FileTransfer {
            source: source_root,
            destination: destination_root,
            size_bytes: source_metadata.len(),
            metadata: MetadataSnapshot::from_metadata(&source_metadata),
        }],
    })
}

fn build_directory_plan(
    operation: Operation,
    source_root: PathBuf,
    destination: PathBuf,
) -> Result<TransferPlan> {
    let destination_root = resolve_directory_destination(&source_root, &destination)?;
    reject_same_path(&source_root, &destination_root)?;
    reject_destination_inside_source(&source_root, &destination_root)?;

    let mut directories = Vec::new();
    let mut files = Vec::new();

    for entry in WalkDir::new(&source_root).follow_links(false) {
        let entry = entry.map_err(ScamError::walk)?;
        let file_type = entry.file_type();

        if file_type.is_symlink() {
            return Err(ScamError::UnsupportedSymlink(entry.path().to_path_buf()));
        }

        let relative = entry
            .path()
            .strip_prefix(&source_root)
            .expect("walkdir entries must stay inside the source root");
        let destination_path = if relative.as_os_str().is_empty() {
            destination_root.clone()
        } else {
            destination_root.join(relative)
        };
        let source_metadata = entry.metadata().map_err(ScamError::walk)?;

        if file_type.is_dir() {
            directories.push(DirectoryTransfer {
                source: entry.path().to_path_buf(),
                destination: destination_path,
                metadata: MetadataSnapshot::from_metadata(&source_metadata),
            });
        } else if file_type.is_file() {
            files.push(FileTransfer {
                source: entry.path().to_path_buf(),
                destination: destination_path,
                size_bytes: source_metadata.len(),
                metadata: MetadataSnapshot::from_metadata(&source_metadata),
            });
        } else {
            return Err(ScamError::UnsupportedSourceType(entry.path().to_path_buf()));
        }
    }

    Ok(TransferPlan {
        operation,
        source_root,
        destination_root,
        directories,
        files,
    })
}

fn resolve_file_destination(source: &Path, destination: &Path) -> Result<PathBuf> {
    match metadata_if_exists(destination)? {
        Some(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err(ScamError::UnsupportedSymlink(destination.to_path_buf()));
            }

            if metadata.is_dir() {
                Ok(destination.join(file_name(source)?))
            } else {
                Ok(destination.to_path_buf())
            }
        }
        None => Ok(destination.to_path_buf()),
    }
}

fn resolve_directory_destination(source: &Path, destination: &Path) -> Result<PathBuf> {
    match metadata_if_exists(destination)? {
        Some(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err(ScamError::UnsupportedSymlink(destination.to_path_buf()));
            }

            if metadata.is_dir() {
                Ok(destination.join(file_name(source)?))
            } else {
                Err(ScamError::DirectoryIntoFile(destination.to_path_buf()))
            }
        }
        None => Ok(destination.to_path_buf()),
    }
}

fn reject_same_path(source: &Path, destination: &Path) -> Result<()> {
    if let Some(destination_metadata) = metadata_if_exists(destination)? {
        if destination_metadata.file_type().is_symlink() {
            return Err(ScamError::UnsupportedSymlink(destination.to_path_buf()));
        }

        let same_file = same_file::is_same_file(source, destination).map_err(|source_error| {
            ScamError::io(
                "comparing source and destination",
                destination,
                source_error,
            )
        })?;
        if same_file {
            let absolute_destination = absolute(destination).map_err(|source_error| {
                ScamError::io("resolving destination path", destination, source_error)
            })?;
            return Err(ScamError::SamePath(absolute_destination));
        }
    }

    let absolute_source = absolute(source)
        .map_err(|source_error| ScamError::io("resolving source path", source, source_error))?;
    let absolute_destination = absolute(destination).map_err(|source_error| {
        ScamError::io("resolving destination path", destination, source_error)
    })?;

    if absolute_source == absolute_destination {
        return Err(ScamError::SamePath(absolute_source));
    }

    Ok(())
}

fn reject_destination_inside_source(source: &Path, destination: &Path) -> Result<()> {
    let absolute_source = absolute(source)
        .map_err(|source_error| ScamError::io("resolving source path", source, source_error))?;
    let absolute_destination = absolute(destination).map_err(|source_error| {
        ScamError::io("resolving destination path", destination, source_error)
    })?;

    if absolute_destination.starts_with(&absolute_source) {
        return Err(ScamError::DestinationInsideSource {
            source_path: absolute_source,
            destination_path: absolute_destination,
        });
    }

    Ok(())
}

fn metadata_if_exists(path: &Path) -> Result<Option<fs::Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(ScamError::io("reading path metadata", path, source)),
    }
}

fn file_name(path: &Path) -> Result<&std::ffi::OsStr> {
    path.file_name()
        .ok_or_else(|| ScamError::MissingFileName(path.to_path_buf()))
}
