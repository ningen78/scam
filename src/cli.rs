//! Command-line interface parsing.

use std::{path::PathBuf, time::Duration};

use clap::{ArgAction, Parser};

use crate::model::{
    DEFAULT_COPY_BUFFER_SIZE, DEFAULT_HASH_BUFFER_SIZE, ExecutionSettings, Operation,
    default_worker_threads,
};

/// Top-level CLI definition.
#[derive(Debug, Parser)]
#[command(
    name = "scam",
    version,
    about = "Secure copy and move with checksum verification, resumable transfers, and parallel directory copying.",
    long_about = "Secure copy and move with checksum verification, resumable transfers, and parallel directory copying.\n\nThe first positional argument selects the operation:\n  +    copy\n  -    move\n\nFor each file, `scam` hashes the source, copies or resumes the destination, hashes the destination, and retries automatically until both checksums match. Source permissions plus access/modified timestamps are preserved on the destination.",
    after_long_help = "Examples:\n  scam + ./movie.mkv /mnt/backup/movie.mkv --progress\n  scam - ./downloads /mnt/archive/downloads --jobs 8 --verbose\n  scam + ./source.bin ./dest.bin --copy-buffer-size 16777216 --hash-buffer-size 8388608\n\nWhen launching through cargo, remember cargo's own `--` separator:\n  cargo run -- + ./source.bin ./dest.bin"
)]
pub struct Cli {
    /// `+` copies, `-` moves after verification.
    #[arg(value_name = "+|-", allow_hyphen_values = true, value_parser = parse_operation)]
    pub operation: Operation,

    /// Source file or directory.
    #[arg(value_name = "SOURCE")]
    pub source: PathBuf,

    /// Destination file or directory.
    #[arg(value_name = "DESTINATION")]
    pub destination: PathBuf,

    /// Read buffer size in bytes used while hashing files.
    #[arg(long, default_value_t = DEFAULT_HASH_BUFFER_SIZE, value_name = "BYTES", value_parser = parse_positive_usize)]
    pub hash_buffer_size: usize,

    /// Buffer size in bytes used while copying file payloads.
    #[arg(long, default_value_t = DEFAULT_COPY_BUFFER_SIZE, value_name = "BYTES", value_parser = parse_positive_usize)]
    pub copy_buffer_size: usize,

    /// Maximum number of attempts per file. Omit for unlimited retries.
    #[arg(long, value_name = "COUNT", value_parser = parse_positive_usize)]
    pub max_retries: Option<usize>,

    /// Milliseconds to wait between failed attempts.
    #[arg(long, default_value_t = 0, value_name = "MILLISECONDS")]
    pub retry_delay_ms: u64,

    /// Number of worker threads used for directory file transfers.
    #[arg(long, default_value_t = default_worker_threads(), value_name = "COUNT", value_parser = parse_positive_usize)]
    pub jobs: usize,

    /// Print aggregate live progress to stderr. Useful for large transfers.
    #[arg(long, action = ArgAction::SetTrue)]
    pub progress: bool,

    /// Print a success summary to stderr after completion.
    #[arg(short, long, action = ArgAction::SetTrue)]
    pub verbose: bool,
}

impl Cli {
    /// Convert the parsed CLI options into runtime execution settings.
    pub fn execution_settings(&self) -> ExecutionSettings {
        ExecutionSettings {
            hash_buffer_size: self.hash_buffer_size,
            copy_buffer_size: self.copy_buffer_size,
            max_retries: self.max_retries,
            retry_delay: Duration::from_millis(self.retry_delay_ms),
            verbose: self.verbose,
            progress: self.progress,
            jobs: self.jobs,
        }
    }
}

fn parse_operation(value: &str) -> Result<Operation, String> {
    match value {
        "+" => Ok(Operation::Copy),
        "-" => Ok(Operation::Move),
        other => Err(format!(
            "expected `+` for copy or `-` for move, got `{other}`"
        )),
    }
}

fn parse_positive_usize(value: &str) -> Result<usize, String> {
    let parsed = value
        .parse::<usize>()
        .map_err(|_| format!("`{value}` is not a valid positive integer"))?;

    if parsed == 0 {
        return Err("value must be greater than zero".to_string());
    }

    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_copy_marker() {
        let cli = Cli::try_parse_from(["scam", "+", "a", "b"]).expect("copy marker should parse");
        assert_eq!(cli.operation, Operation::Copy);
    }

    #[test]
    fn parses_move_marker() {
        let cli = Cli::try_parse_from(["scam", "-", "a", "b"]).expect("move marker should parse");
        assert_eq!(cli.operation, Operation::Move);
    }

    #[test]
    fn parses_progress_and_jobs_options() {
        let cli = Cli::try_parse_from([
            "scam",
            "+",
            "a",
            "b",
            "--progress",
            "--jobs",
            "4",
            "--copy-buffer-size",
            "65536",
        ])
        .expect("extended options should parse");

        assert!(cli.progress);
        assert_eq!(cli.jobs, 4);
        assert_eq!(cli.copy_buffer_size, 65_536);
    }
}
