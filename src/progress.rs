//! Optional live progress reporting.
//!
//! Progress output is intentionally aggregate rather than per-file so it stays
//! readable even when directory transfers run with many parallel worker threads.

use std::{
    io::{self, Write},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crate::{
    error::{Result, ScamError},
    model::DEFAULT_PROGRESS_UPDATE_INTERVAL,
};

/// Lifecycle wrapper around the background progress-rendering thread.
pub struct ProgressRuntime {
    reporter: Arc<ProgressReporter>,
    worker: Option<JoinHandle<()>>,
}

impl ProgressRuntime {
    /// Create a progress reporter when the user enabled progress output.
    pub fn new(enabled: bool, total_bytes: u64, total_files: usize) -> Result<Option<Self>> {
        if !enabled || total_files == 0 {
            return Ok(None);
        }

        let reporter = Arc::new(ProgressReporter::new(total_bytes, total_files));
        let worker_reporter = Arc::clone(&reporter);
        let worker = thread::Builder::new()
            .name("scam-progress".to_string())
            .spawn(move || render_loop(worker_reporter))
            .map_err(|source| ScamError::thread_spawn("progress reporter", source))?;

        Ok(Some(Self {
            reporter,
            worker: Some(worker),
        }))
    }

    /// Obtain a cloneable shared reporter handle for worker code.
    pub fn reporter(&self) -> Arc<ProgressReporter> {
        Arc::clone(&self.reporter)
    }

    /// Stop the background thread and emit a final 100% progress line.
    pub fn finish_success(&mut self) -> Result<()> {
        self.reporter.mark_success();
        self.reporter.stop();

        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| ScamError::background_task_panicked("progress reporter"))?;
        }

        Ok(())
    }
}

impl Drop for ProgressRuntime {
    fn drop(&mut self) {
        self.reporter.stop();

        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Thread-safe shared progress state.
pub struct ProgressReporter {
    total_bytes: u64,
    total_files: usize,
    started_at: Instant,
    position_bytes: AtomicU64,
    completed_files: AtomicUsize,
    retries: AtomicUsize,
    stopped: AtomicBool,
    success: AtomicBool,
    print_lock: Mutex<()>,
}

impl ProgressReporter {
    fn new(total_bytes: u64, total_files: usize) -> Self {
        Self {
            total_bytes,
            total_files,
            started_at: Instant::now(),
            position_bytes: AtomicU64::new(0),
            completed_files: AtomicUsize::new(0),
            retries: AtomicUsize::new(0),
            stopped: AtomicBool::new(false),
            success: AtomicBool::new(false),
            print_lock: Mutex::new(()),
        }
    }

    /// Start tracking progress for one file.
    pub fn begin_file(self: &Arc<Self>, file_size: u64) -> FileProgress {
        FileProgress {
            reporter: Arc::clone(self),
            file_size,
            accounted_bytes: 0,
            completed: false,
        }
    }

    /// Log a retry message and update the live retry counter.
    pub fn note_retry(&self, path: &Path, attempt: usize, reason: &str) {
        self.retries.fetch_add(1, Ordering::Relaxed);
        self.print_line(&format!(
            "Retry {attempt} for `{}`: {reason}",
            path.display()
        ));
    }

    fn add_bytes(&self, bytes: u64) {
        if bytes != 0 {
            self.position_bytes.fetch_add(bytes, Ordering::Relaxed);
        }
    }

    fn remove_bytes(&self, bytes: u64) {
        if bytes != 0 {
            self.position_bytes.fetch_sub(bytes, Ordering::Relaxed);
        }
    }

    fn complete_file(&self) {
        self.completed_files.fetch_add(1, Ordering::Relaxed);
    }

    fn mark_success(&self) {
        self.success.store(true, Ordering::Relaxed);
    }

    fn stop(&self) {
        self.stopped.store(true, Ordering::Relaxed);
    }

    fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Relaxed)
    }

    fn succeeded(&self) -> bool {
        self.success.load(Ordering::Relaxed)
    }

    fn snapshot(&self) -> ProgressSnapshot {
        ProgressSnapshot {
            position_bytes: self.position_bytes.load(Ordering::Relaxed),
            completed_files: self.completed_files.load(Ordering::Relaxed),
            retries: self.retries.load(Ordering::Relaxed),
        }
    }

    fn print_snapshot(&self, snapshot: ProgressSnapshot) {
        self.print_line(&format_snapshot(
            snapshot,
            self.total_bytes,
            self.total_files,
            self.started_at.elapsed(),
        ));
    }

    fn print_line(&self, line: &str) {
        let _guard = self
            .print_lock
            .lock()
            .expect("progress print mutex should not be poisoned");
        let mut stderr = io::stderr().lock();
        let _ = writeln!(stderr, "{line}");
    }
}

/// Per-file progress bookkeeping that can be rolled back if a retry is needed.
pub struct FileProgress {
    reporter: Arc<ProgressReporter>,
    file_size: u64,
    accounted_bytes: u64,
    completed: bool,
}

impl FileProgress {
    /// Mark bytes that can be reused from an already matching destination prefix.
    pub fn account_resume(&mut self, bytes: u64) {
        self.reporter.add_bytes(bytes);
        self.accounted_bytes += bytes;
    }

    /// Mark newly copied bytes.
    pub fn account_copy(&mut self, bytes: u64) {
        self.reporter.add_bytes(bytes);
        self.accounted_bytes += bytes;
    }

    /// Reset the file's accounted progress before another retry attempt.
    pub fn reset_attempt(&mut self) {
        self.reporter.remove_bytes(self.accounted_bytes);
        self.accounted_bytes = 0;
    }

    /// Mark the file as fully completed.
    pub fn complete(&mut self) {
        if self.completed {
            return;
        }

        if self.accounted_bytes < self.file_size {
            self.reporter
                .add_bytes(self.file_size - self.accounted_bytes);
            self.accounted_bytes = self.file_size;
        }

        self.completed = true;
        self.reporter.complete_file();
    }
}

impl Drop for FileProgress {
    fn drop(&mut self) {
        if !self.completed {
            self.reporter.remove_bytes(self.accounted_bytes);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct ProgressSnapshot {
    position_bytes: u64,
    completed_files: usize,
    retries: usize,
}

fn render_loop(reporter: Arc<ProgressReporter>) {
    let mut last_snapshot = ProgressSnapshot::default();

    loop {
        thread::sleep(DEFAULT_PROGRESS_UPDATE_INTERVAL);

        let snapshot = reporter.snapshot();
        if snapshot != last_snapshot {
            reporter.print_snapshot(snapshot);
            last_snapshot = snapshot;
        }

        if reporter.is_stopped() {
            break;
        }
    }

    if reporter.succeeded() {
        let final_snapshot = reporter.snapshot();
        if final_snapshot != last_snapshot {
            reporter.print_snapshot(final_snapshot);
        }
    }
}

fn format_snapshot(
    snapshot: ProgressSnapshot,
    total_bytes: u64,
    total_files: usize,
    elapsed: Duration,
) -> String {
    let percent = if total_bytes > 0 {
        (snapshot.position_bytes as f64 / total_bytes as f64) * 100.0
    } else if total_files > 0 {
        (snapshot.completed_files as f64 / total_files as f64) * 100.0
    } else {
        100.0
    };
    let elapsed_seconds = elapsed.as_secs_f64().max(0.001);
    let throughput = snapshot.position_bytes as f64 / elapsed_seconds;

    format!(
        "Progress: {percent:.1}% | {}/{} files | {}/{} | {} {} | {}/s",
        snapshot.completed_files,
        total_files,
        format_bytes(snapshot.position_bytes),
        format_bytes(total_bytes),
        snapshot.retries,
        pluralize(snapshot.retries, "retry", "retries"),
        format_rate(throughput),
    )
}

fn format_rate(bytes_per_second: f64) -> String {
    format_bytes(bytes_per_second.max(0.0).round() as u64)
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
