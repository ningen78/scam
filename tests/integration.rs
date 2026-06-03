use std::{
    fs,
    path::Path,
    time::{Duration, SystemTime},
};

use filetime::{FileTime, set_file_mtime};
use scam::{ExecutionSettings, Operation, ScamError, build_transfer_plan, execute_transfer};
use tempfile::TempDir;

#[test]
fn copies_a_file_and_keeps_the_source() {
    let sandbox = TempDir::new().expect("temp dir should be created");
    let source = sandbox.path().join("source.txt");
    let destination = sandbox.path().join("destination.txt");
    fs::write(&source, b"verified copy").expect("source file should be written");

    let plan = build_transfer_plan(Operation::Copy, source.clone(), destination.clone())
        .expect("plan should build");
    let summary =
        execute_transfer(&plan, &ExecutionSettings::default()).expect("copy should succeed");

    assert_eq!(
        fs::read(&source).expect("source should remain"),
        b"verified copy"
    );
    assert_eq!(
        fs::read(&destination).expect("destination should exist"),
        b"verified copy"
    );
    assert_eq!(summary.files, 1);
    assert_eq!(summary.directories, 0);
}

#[test]
fn moves_a_file_and_removes_the_source() {
    let sandbox = TempDir::new().expect("temp dir should be created");
    let source = sandbox.path().join("source.bin");
    let destination = sandbox.path().join("moved.bin");
    fs::write(&source, b"move me").expect("source file should be written");

    let plan = build_transfer_plan(Operation::Move, source.clone(), destination.clone())
        .expect("plan should build");
    let summary =
        execute_transfer(&plan, &ExecutionSettings::default()).expect("move should succeed");

    assert!(
        !source.exists(),
        "source should be removed after a verified move"
    );
    assert_eq!(
        fs::read(&destination).expect("destination should exist"),
        b"move me"
    );
    assert_eq!(summary.files, 1);
}

#[test]
fn copies_a_directory_tree_into_an_existing_directory() {
    let sandbox = TempDir::new().expect("temp dir should be created");
    let source_root = sandbox.path().join("src-tree");
    let nested = source_root.join("nested");
    let empty = source_root.join("empty");
    let destination_parent = sandbox.path().join("dest-parent");

    fs::create_dir_all(&nested).expect("nested directory should exist");
    fs::create_dir_all(&empty).expect("empty directory should exist");
    fs::create_dir_all(&destination_parent).expect("destination parent should exist");
    fs::write(source_root.join("top.txt"), b"top").expect("top-level file should be written");
    fs::write(nested.join("child.txt"), b"child").expect("nested file should be written");

    let plan = build_transfer_plan(
        Operation::Copy,
        source_root.clone(),
        destination_parent.clone(),
    )
    .expect("plan should build");
    let settings = ExecutionSettings {
        jobs: 4,
        ..ExecutionSettings::default()
    };
    let summary = execute_transfer(&plan, &settings).expect("copy should succeed");

    let copied_root = destination_parent.join("src-tree");
    assert_eq!(
        fs::read(copied_root.join("top.txt")).expect("top file should exist"),
        b"top"
    );
    assert_eq!(
        fs::read(copied_root.join("nested").join("child.txt")).expect("nested file should exist"),
        b"child"
    );
    assert!(
        copied_root.join("empty").is_dir(),
        "empty directories should be preserved"
    );
    assert!(
        source_root.exists(),
        "copy should leave the source tree intact"
    );
    assert_eq!(summary.files, 2);
    assert!(
        summary.directories >= 3,
        "root, nested, and empty directories should be counted"
    );
}

#[test]
fn copies_many_files_with_parallel_workers() {
    let sandbox = TempDir::new().expect("temp dir should be created");
    let source_root = sandbox.path().join("bulk");
    let destination_root = sandbox.path().join("bulk-copy");
    fs::create_dir_all(&source_root).expect("source directory should exist");

    for index in 0..32 {
        fs::write(
            source_root.join(format!("file-{index:02}.bin")),
            patterned_bytes(32 * 1024 + index),
        )
        .expect("test file should be written");
    }

    let plan = build_transfer_plan(
        Operation::Copy,
        source_root.clone(),
        destination_root.clone(),
    )
    .expect("plan should build");
    let settings = ExecutionSettings {
        jobs: 4,
        ..ExecutionSettings::default()
    };
    let summary = execute_transfer(&plan, &settings).expect("parallel copy should succeed");

    for index in 0..32 {
        let file_name = format!("file-{index:02}.bin");
        assert_eq!(
            fs::read(source_root.join(&file_name)).expect("source file should exist"),
            fs::read(destination_root.join(&file_name)).expect("destination file should exist")
        );
    }
    assert_eq!(summary.files, 32);
}

#[test]
fn moves_a_directory_tree_and_removes_the_source_directories() {
    let sandbox = TempDir::new().expect("temp dir should be created");
    let source_root = sandbox.path().join("photos");
    let nested = source_root.join("2026");
    let destination_root = sandbox.path().join("archive");

    fs::create_dir_all(&nested).expect("nested directory should exist");
    fs::write(source_root.join("cover.txt"), b"cover").expect("cover file should be written");
    fs::write(nested.join("shot.txt"), b"shot").expect("nested file should be written");

    let plan = build_transfer_plan(
        Operation::Move,
        source_root.clone(),
        destination_root.clone(),
    )
    .expect("plan should build");
    let settings = ExecutionSettings {
        jobs: 4,
        ..ExecutionSettings::default()
    };
    let summary = execute_transfer(&plan, &settings).expect("move should succeed");

    assert!(
        !source_root.exists(),
        "source tree should be removed after move"
    );
    assert_eq!(
        fs::read(destination_root.join("cover.txt")).expect("cover should exist"),
        b"cover"
    );
    assert_eq!(
        fs::read(destination_root.join("2026").join("shot.txt")).expect("nested file should exist"),
        b"shot"
    );
    assert_eq!(summary.files, 2);
}

#[test]
fn resumes_from_a_matching_partial_destination() {
    let sandbox = TempDir::new().expect("temp dir should be created");
    let source = sandbox.path().join("large.bin");
    let destination = sandbox.path().join("large-copy.bin");
    let payload = patterned_bytes(256 * 1024);
    let resumed_len = 96 * 1024;

    fs::write(&source, &payload).expect("source file should be written");
    fs::write(&destination, &payload[..resumed_len])
        .expect("partial destination should be written");

    let plan = build_transfer_plan(Operation::Copy, source.clone(), destination.clone())
        .expect("plan should build");
    let settings = ExecutionSettings {
        copy_buffer_size: 64 * 1024,
        ..ExecutionSettings::default()
    };
    let summary = execute_transfer(&plan, &settings).expect("resume should succeed");

    assert_eq!(
        fs::read(&destination).expect("destination should exist"),
        payload
    );
    assert_eq!(summary.resumed_files, 1);
    assert_eq!(summary.resumed_bytes, resumed_len as u64);
}

#[test]
fn preserves_file_permissions_and_modified_time() {
    let sandbox = TempDir::new().expect("temp dir should be created");
    let source = sandbox.path().join("metadata.txt");
    let destination = sandbox.path().join("metadata-copy.txt");
    let expected_modified = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);

    fs::write(&source, b"metadata").expect("source file should be written");
    set_file_mtime(&source, FileTime::from_system_time(expected_modified))
        .expect("source mtime should be set");
    set_readonly(&source, true);

    let plan = build_transfer_plan(Operation::Copy, source.clone(), destination.clone())
        .expect("plan should build");
    execute_transfer(&plan, &ExecutionSettings::default()).expect("copy should succeed");

    let destination_metadata =
        fs::metadata(&destination).expect("destination metadata should exist");
    assert!(
        destination_metadata.permissions().readonly(),
        "destination should preserve the read-only bit"
    );
    assert_time_close(
        destination_metadata
            .modified()
            .expect("destination modified time should be readable"),
        expected_modified,
    );

    set_readonly(&source, false);
    set_readonly(&destination, false);
}

#[test]
fn preserves_directory_modified_time_after_recursive_copy() {
    let sandbox = TempDir::new().expect("temp dir should be created");
    let source_root = sandbox.path().join("dir-meta");
    let destination_root = sandbox.path().join("dir-meta-copy");
    let expected_modified = SystemTime::UNIX_EPOCH + Duration::from_secs(1_710_000_000);

    fs::create_dir_all(source_root.join("nested")).expect("nested directory should be created");
    fs::write(
        source_root.join("nested").join("note.txt"),
        b"directory metadata",
    )
    .expect("child file should be written");
    set_file_mtime(&source_root, FileTime::from_system_time(expected_modified))
        .expect("source directory mtime should be set");

    let plan = build_transfer_plan(
        Operation::Copy,
        source_root.clone(),
        destination_root.clone(),
    )
    .expect("plan should build");
    execute_transfer(&plan, &ExecutionSettings::default()).expect("copy should succeed");

    let destination_metadata =
        fs::metadata(&destination_root).expect("destination directory metadata should exist");
    assert_time_close(
        destination_metadata
            .modified()
            .expect("destination modified time should be readable"),
        expected_modified,
    );
}

#[test]
fn rejects_directory_destination_inside_source_tree() {
    let sandbox = TempDir::new().expect("temp dir should be created");
    let source_root = sandbox.path().join("source");
    fs::create_dir_all(&source_root).expect("source directory should exist");

    let result = build_transfer_plan(
        Operation::Copy,
        source_root.clone(),
        source_root.join("nested").join("destination"),
    );

    match result {
        Err(ScamError::DestinationInsideSource { .. }) => {}
        other => panic!("expected DestinationInsideSource, got {other:?}"),
    }
}

#[test]
fn copies_a_file_into_an_existing_directory() {
    let sandbox = TempDir::new().expect("temp dir should be created");
    let source = sandbox.path().join("report.txt");
    let destination_dir = sandbox.path().join("out");
    fs::write(&source, b"report").expect("source file should be written");
    fs::create_dir_all(&destination_dir).expect("destination directory should exist");

    let plan = build_transfer_plan(Operation::Copy, source.clone(), destination_dir.clone())
        .expect("plan should build");
    execute_transfer(&plan, &ExecutionSettings::default()).expect("copy should succeed");

    assert_eq!(
        fs::read(destination_dir.join("report.txt")).expect("destination file should exist"),
        b"report"
    );
}

fn patterned_bytes(length: usize) -> Vec<u8> {
    (0..length).map(|index| (index % 251) as u8).collect()
}

fn assert_time_close(actual: SystemTime, expected: SystemTime) {
    let difference = actual.duration_since(expected).unwrap_or_else(|_| {
        expected
            .duration_since(actual)
            .expect("times should be comparable")
    });
    assert!(
        difference <= Duration::from_secs(2),
        "expected times to be within 2 seconds, but they differed by {difference:?}"
    );
}

fn set_readonly(path: &Path, readonly: bool) {
    let mut permissions = fs::metadata(path)
        .expect("metadata should be available")
        .permissions();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        if readonly {
            permissions.set_mode(permissions.mode() & !0o222);
        } else {
            permissions.set_mode(permissions.mode() | 0o200);
        }
    }

    #[cfg(not(unix))]
    {
        permissions.set_readonly(readonly);
    }

    fs::set_permissions(path, permissions).expect("permissions should be updated");
}
