//! Transfer execution orchestration.
//!
//! The engine creates destination directories, runs verified file transfers in
//! parallel when beneficial, restores directory metadata, and finally removes
//! source directories after successful move operations.

use std::{fs, sync::Arc, time::Instant};

use rayon::prelude::*;

use crate::{
    copy::{FileSummary, transfer_file_with_verification},
    error::{Result, ScamError},
    model::{DirectoryTransfer, ExecutionSettings, RunSummary, TransferPlan},
    progress::{ProgressReporter, ProgressRuntime},
};

/// Execute a fully resolved transfer plan.
pub fn execute_transfer(plan: &TransferPlan, settings: &ExecutionSettings) -> Result<RunSummary> {
    let started_at = Instant::now();
    let mut progress =
        ProgressRuntime::new(settings.progress, plan.total_bytes(), plan.files.len())?;

    if plan.is_directory_transfer() {
        create_destination_directories(&plan.directories)?;
    }

    let progress_reporter = progress.as_ref().map(ProgressRuntime::reporter);
    let file_totals = transfer_files(plan, settings, progress_reporter.as_ref())?;

    if plan.is_directory_transfer() {
        apply_destination_directory_metadata(&plan.directories)?;
    }

    if plan.operation.is_move() && plan.is_directory_transfer() {
        remove_source_directories(&plan.directories)?;
    }

    if let Some(progress) = progress.as_mut() {
        progress.finish_success()?;
    }

    let summary = RunSummary {
        directories: plan.directories.len(),
        files: plan.files.len(),
        bytes_copied: file_totals.bytes_copied,
        resumed_files: file_totals.resumed_files,
        resumed_bytes: file_totals.resumed_bytes,
        retries: file_totals.retries,
        elapsed: started_at.elapsed(),
    };

    if settings.verbose {
        print_summary(plan, settings, &summary);
    }

    Ok(summary)
}

#[derive(Debug, Default, Clone, Copy)]
struct TransferTotals {
    bytes_copied: u64,
    resumed_bytes: u64,
    resumed_files: usize,
    retries: usize,
}

impl TransferTotals {
    fn add_file(&mut self, file_summary: FileSummary) {
        self.bytes_copied += file_summary.bytes_copied;
        self.resumed_bytes += file_summary.resumed_bytes;
        self.retries += file_summary.retries;
        if file_summary.resumed_bytes > 0 {
            self.resumed_files += 1;
        }
    }
}

fn transfer_files(
    plan: &TransferPlan,
    settings: &ExecutionSettings,
    progress: Option<&Arc<ProgressReporter>>,
) -> Result<TransferTotals> {
    if plan.files.is_empty() {
        return Ok(TransferTotals::default());
    }

    let move_after_success = plan.operation.is_move();
    let worker_count = if plan.files.len() > 1 {
        settings.jobs.max(1)
    } else {
        1
    };

    let results = if worker_count == 1 {
        plan.files
            .iter()
            .map(|file| {
                transfer_file_with_verification(file, move_after_success, settings, progress)
            })
            .collect::<Vec<_>>()
    } else {
        let progress = progress.cloned();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(worker_count)
            .thread_name(|index| format!("scam-worker-{index}"))
            .build()
            .map_err(ScamError::parallel_setup)?;

        pool.install(|| {
            plan.files
                .par_iter()
                .map(|file| {
                    transfer_file_with_verification(
                        file,
                        move_after_success,
                        settings,
                        progress.as_ref(),
                    )
                })
                .collect::<Vec<_>>()
        })
    };

    let mut totals = TransferTotals::default();
    for result in results {
        totals.add_file(result?);
    }

    Ok(totals)
}

fn create_destination_directories(directories: &[DirectoryTransfer]) -> Result<()> {
    for directory in directories {
        if let Ok(metadata) = fs::symlink_metadata(&directory.destination)
            && metadata.file_type().is_symlink()
        {
            return Err(ScamError::UnsupportedSymlink(directory.destination.clone()));
        }

        fs::create_dir_all(&directory.destination).map_err(|source| {
            ScamError::io(
                "creating destination directory",
                &directory.destination,
                source,
            )
        })?;
    }

    Ok(())
}

fn apply_destination_directory_metadata(directories: &[DirectoryTransfer]) -> Result<()> {
    for directory in directories.iter().rev() {
        directory.metadata.apply_to_path(&directory.destination)?;
    }

    Ok(())
}

fn remove_source_directories(directories: &[DirectoryTransfer]) -> Result<()> {
    for directory in directories.iter().rev() {
        fs::remove_dir(&directory.source).map_err(|source| {
            ScamError::io(
                "removing emptied source directory",
                &directory.source,
                source,
            )
        })?;
    }

    Ok(())
}

fn print_summary(plan: &TransferPlan, settings: &ExecutionSettings, summary: &RunSummary) {
    let throughput = if summary.elapsed.is_zero() {
        0.0
    } else {
        summary.bytes_copied as f64 / summary.elapsed.as_secs_f64()
    };

    let mut details = format!(
        "{} {} {}, {} {}, {} in {:.3}s ({}/s) with {} {}",
        plan.operation.past_tense(),
        summary.files,
        pluralize(summary.files, "file", "files"),
        summary.directories,
        pluralize(summary.directories, "directory", "directories"),
        format_bytes(summary.bytes_copied),
        summary.elapsed.as_secs_f64(),
        format_bytes(throughput.round() as u64),
        summary.retries,
        pluralize(summary.retries, "retry", "retries"),
    );

    if plan.files.len() > 1 && settings.jobs > 1 {
        details.push_str(&format!(
            " using {} {}",
            settings.jobs,
            pluralize(settings.jobs, "job", "jobs")
        ));
    }

    details.push('.');
    eprintln!("{details}");

    if summary.resumed_files > 0 {
        eprintln!(
            "Resumed {} {} totaling {}.",
            summary.resumed_files,
            pluralize(summary.resumed_files, "file", "files"),
            format_bytes(summary.resumed_bytes),
        );
    }
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];

    let mut value = bytes as f64;
    let mut unit_index = 0usize;
    while value >= 1024.0 && unit_index + 1 < UNITS.len() {
        value /= 1024.0;
        unit_index += 1;
    }

    if unit_index == 0 {
        format!("{bytes} {}", UNITS[unit_index])
    } else {
        format!("{value:.2} {}", UNITS[unit_index])
    }
}

fn pluralize<'a>(count: usize, singular: &'a str, plural: &'a str) -> &'a str {
    if count == 1 { singular } else { plural }
}
