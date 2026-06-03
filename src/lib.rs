//! `scam` — secure copy and move with checksum verification.
//!
//! The crate exposes the command-line entry point used by the binary in
//! [`main`](crate::run), plus the internal building blocks that make testing and
//! future extension straightforward.

pub mod checksum;
pub mod cli;
pub mod copy;
pub mod engine;
pub mod error;
pub mod metadata;
pub mod model;
pub mod plan;
pub mod progress;

use clap::Parser;
use cli::Cli;

pub use engine::execute_transfer;
pub use error::{Result, ScamError};
pub use model::{ExecutionSettings, Operation, RunSummary, TransferPlan, default_worker_threads};
pub use plan::build_transfer_plan;

/// Parse command-line arguments, build a transfer plan, and execute it.
pub fn run() -> Result<RunSummary> {
    let cli = Cli::parse();
    let plan = build_transfer_plan(cli.operation, cli.source.clone(), cli.destination.clone())?;
    execute_transfer(&plan, &cli.execution_settings())
}
