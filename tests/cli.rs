use std::{fs, process::Command};

use assert_cmd::prelude::*;
use predicates::prelude::*;
use tempfile::TempDir;

#[test]
fn emits_progress_output_when_requested() {
    let sandbox = TempDir::new().expect("temp dir should be created");
    let source = sandbox.path().join("progress.bin");
    let destination = sandbox.path().join("progress-copy.bin");
    let payload: Vec<u8> = (0..(256 * 1024)).map(|index| (index % 251) as u8).collect();
    fs::write(&source, payload).expect("source file should be written");

    let mut command = Command::cargo_bin("scam").expect("binary should build for tests");
    command
        .arg("+")
        .arg(&source)
        .arg(&destination)
        .arg("--progress")
        .assert()
        .success()
        .stderr(predicate::str::contains("Progress:"));

    assert_eq!(
        fs::read(&destination).expect("destination file should exist"),
        fs::read(&source).expect("source file should still exist")
    );
}
