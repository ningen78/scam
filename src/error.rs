//! Error types for `scam`.

use std::{io, path::PathBuf};

use thiserror::Error;

/// Crate-local result type.
pub type Result<T> = std::result::Result<T, ScamError>;

/// Errors that can occur while planning or executing a transfer.
#[derive(Debug, Error)]
pub enum ScamError {
    /// Any ordinary filesystem I/O failure.
    #[error("I/O error while {action} `{path}`: {source}")]
    Io {
        /// What the code was trying to do.
        action: String,
        /// Which path triggered the error.
        path: PathBuf,
        /// The original operating-system error.
        #[source]
        source: io::Error,
    },

    /// Recursive directory traversal failed.
    #[error("filesystem walk failed at `{path}`: {source}")]
    Walk {
        /// Path associated with the walk failure when one is available.
        path: PathBuf,
        /// The original walkdir error.
        #[source]
        source: walkdir::Error,
    },

    /// The CLI parser already validated the syntax, but planning can still fail
    /// if a path does not have a usable file name.
    #[error("could not derive a file name from `{0}`")]
    MissingFileName(PathBuf),

    /// The tool is intentionally conservative around symlinks because a secure
    /// copy/move tool should not silently follow them to unexpected targets.
    #[error("symbolic links are not supported: `{0}`")]
    UnsupportedSymlink(PathBuf),

    /// Only regular files and directories are supported.
    #[error("unsupported source type at `{0}`; only regular files and directories are supported")]
    UnsupportedSourceType(PathBuf),

    /// Copying a directory into an existing file is never meaningful.
    #[error("cannot copy a directory into the existing file `{0}`")]
    DirectoryIntoFile(PathBuf),

    /// A file transfer resolved to a path that unexpectedly became a directory.
    #[error("destination path is a directory where a file is required: `{0}`")]
    DestinationIsDirectory(PathBuf),

    /// The source and destination resolve to the same location.
    #[error("source and destination resolve to the same path: `{0}`")]
    SamePath(PathBuf),

    /// Recursive operations may not place the destination inside the source.
    #[error(
        "refusing to place destination `{destination_path}` inside source directory `{source_path}`"
    )]
    DestinationInsideSource {
        /// Source directory root.
        source_path: PathBuf,
        /// Conflicting destination path.
        destination_path: PathBuf,
    },

    /// The user asked for a bounded retry count and it was exhausted.
    #[error("exhausted {attempts} attempt(s) while copying `{path}`")]
    RetryLimitExceeded {
        /// Source file that never verified successfully.
        path: PathBuf,
        /// How many attempts were made.
        attempts: usize,
    },

    /// A move never deletes the source after it has changed since the checksum
    /// that was used for verification.
    #[error("source file changed while move was in progress: `{0}`")]
    SourceChanged(PathBuf),

    /// Parallel worker-pool initialization failed.
    #[error("failed to initialize the parallel worker pool: {source}")]
    ParallelSetup {
        /// The original Rayon thread-pool build error.
        #[source]
        source: rayon::ThreadPoolBuildError,
    },

    /// Starting a background helper thread failed.
    #[error("failed to start the {task}: {source}")]
    ThreadSpawn {
        /// Human-friendly name of the failed thread.
        task: &'static str,
        /// The original operating-system error.
        #[source]
        source: io::Error,
    },

    /// A background helper thread panicked unexpectedly.
    #[error("the {0} panicked unexpectedly")]
    BackgroundTaskPanicked(&'static str),
}

impl ScamError {
    /// Helper constructor for I/O failures.
    pub fn io(action: impl Into<String>, path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Io {
            action: action.into(),
            path: path.into(),
            source,
        }
    }

    /// Helper constructor for walkdir failures.
    pub fn walk(source: walkdir::Error) -> Self {
        let path = source.path().map_or_else(PathBuf::new, PathBuf::from);
        Self::Walk { path, source }
    }

    /// Helper constructor for Rayon thread-pool setup failures.
    pub fn parallel_setup(source: rayon::ThreadPoolBuildError) -> Self {
        Self::ParallelSetup { source }
    }

    /// Helper constructor for helper-thread startup failures.
    pub fn thread_spawn(task: &'static str, source: io::Error) -> Self {
        Self::ThreadSpawn { task, source }
    }

    /// Helper constructor for unexpected background thread panics.
    pub fn background_task_panicked(task: &'static str) -> Self {
        Self::BackgroundTaskPanicked(task)
    }
}
