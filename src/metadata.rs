//! Metadata capture and restoration.
//!
//! `scam` preserves the source item's permissions plus the timestamps that are
//! widely available through the Rust standard library: access time and modified
//! time. Capturing those values before any copy I/O starts means destination
//! metadata can be restored even when a move later deletes the source.

use std::{
    fs::{self, Metadata, Permissions},
    path::Path,
    time::SystemTime,
};

use filetime::{FileTime, set_file_atime, set_file_mtime, set_file_times};

use crate::error::{Result, ScamError};

/// Snapshot of source metadata that can be restored onto a destination path.
#[derive(Debug, Clone)]
pub struct MetadataSnapshot {
    /// Portable permission information, including the Windows read-only bit and
    /// Unix mode bits.
    pub permissions: Permissions,
    /// Access timestamp when the platform exposes it.
    pub accessed: Option<SystemTime>,
    /// Modification timestamp when the platform exposes it.
    pub modified: Option<SystemTime>,
}

impl MetadataSnapshot {
    /// Build a snapshot from already-loaded filesystem metadata.
    pub fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            permissions: metadata.permissions(),
            accessed: metadata.accessed().ok(),
            modified: metadata.modified().ok(),
        }
    }

    /// Capture metadata directly from a filesystem path.
    pub fn capture(path: &Path) -> Result<Self> {
        let metadata = fs::metadata(path)
            .map_err(|source| ScamError::io("reading metadata for preservation", path, source))?;
        Ok(Self::from_metadata(&metadata))
    }

    /// Apply the preserved timestamps and permissions to a destination path.
    pub fn apply_to_path(&self, path: &Path) -> Result<()> {
        temporarily_make_writable(path)?;
        self.apply_timestamps(path)?;
        fs::set_permissions(path, self.permissions.clone())
            .map_err(|source| ScamError::io("preserving permissions", path, source))
    }

    fn apply_timestamps(&self, path: &Path) -> Result<()> {
        match (self.accessed, self.modified) {
            (Some(accessed), Some(modified)) => set_file_times(
                path,
                FileTime::from_system_time(accessed),
                FileTime::from_system_time(modified),
            )
            .map_err(|source| ScamError::io("preserving timestamps", path, source)),
            (Some(accessed), None) => set_file_atime(path, FileTime::from_system_time(accessed))
                .map_err(|source| ScamError::io("preserving access time", path, source)),
            (None, Some(modified)) => set_file_mtime(path, FileTime::from_system_time(modified))
                .map_err(|source| ScamError::io("preserving modification time", path, source)),
            (None, None) => Ok(()),
        }
    }
}

fn temporarily_make_writable(path: &Path) -> Result<()> {
    let metadata = fs::metadata(path).map_err(|source| {
        ScamError::io(
            "reading destination metadata for preservation",
            path,
            source,
        )
    })?;
    let mut permissions = metadata.permissions();

    if permissions.readonly() {
        clear_readonly(&mut permissions);
        fs::set_permissions(path, permissions)
            .map_err(|source| ScamError::io("temporarily clearing read-only bit", path, source))?;
    }

    Ok(())
}

#[cfg(unix)]
fn clear_readonly(permissions: &mut Permissions) {
    use std::os::unix::fs::PermissionsExt;

    permissions.set_mode(permissions.mode() | 0o200);
}

#[cfg(not(unix))]
fn clear_readonly(permissions: &mut Permissions) {
    permissions.set_readonly(false);
}
