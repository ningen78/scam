# scam

`scam` is a tongue-in-cheek acronym for **secure copy and move**.

It behaves like a focused `cp` / `mv`-style tool for files and directories, with one non-negotiable rule: **every copied file must verify successfully before the operation is considered complete**.

## Features

- verified copy and move using **BLAKE3** checksums,
- automatic retry loop for failed or mismatched copies,
- **recursive directory transfers**,
- **parallel directory file copying** with configurable worker count,
- **resumable partial-copy handling** when a matching destination prefix already exists,
- **preserved metadata** for files and directories:
  - permissions / read-only bit,
  - access time when available,
  - modification time when available,
- optional aggregate **live progress output** for large transfers.

## What happens for each file

`scam` processes each file in this order:

1. Capture source metadata that should be preserved on the destination.
2. Compute a BLAKE3 checksum of the source file.
3. Inspect the destination:
   - if it is already complete and checksum-identical, reuse it,
   - if it contains a matching prefix, resume from that offset,
   - otherwise restart from zero.
4. Copy the remaining payload.
5. Compute a BLAKE3 checksum of the destination file.
6. Compare both checksums.
7. If they differ, retry the file automatically.
8. Restore preserved metadata on the destination.
9. For move operations, re-check the source before deleting it.

## Why BLAKE3?

BLAKE3 is a strong fit here because it is:

- cryptographically strong,
- very fast in practice,
- well suited to high-throughput integrity verification.

## Supported operations

The first argument selects the operation:

- `+` → copy
- `-` → move

Examples:

```powershell
scam.exe + .\source.iso D:\backup\source.iso
scam.exe - .\downloads D:\archive\downloads
```

When invoking through Cargo, remember Cargo's own argument separator:

```powershell
cargo run -- + .\source.iso D:\backup\source.iso
cargo run -- - .\downloads D:\archive\downloads
```

## Path behavior

`scam` resolves paths similarly to `cp` and `mv`:

- copying a **file** to an existing directory places the file inside that directory,
- copying a **file** to a non-directory path writes exactly that file path,
- copying a **directory** to an existing directory creates `destination\source_dir_name`,
- copying a **directory** to a non-existent path creates that directory tree there.

Recursive directory transfers are supported.

## Safety rules

To stay conservative and predictable, `scam` currently:

- supports regular files and directories,
- rejects symbolic links instead of following them,
- refuses to copy a directory into itself or into one of its own descendants,
- refuses to copy onto the same file.

## Performance notes

`scam` aims to stay fast while still verifying every result:

- Fresh, non-progress copies can use the operating system's fast copy path.
- Recursive directory transfers can copy files in parallel with `--jobs`.
- Resume support avoids re-copying bytes that are already present and match the source.
- The release profile uses fat LTO and a single codegen unit to bias the final binary toward runtime speed.

## CLI

```text
scam <+|-> <SOURCE> <DESTINATION> [OPTIONS]

Options:
  --hash-buffer-size <BYTES>   Read buffer used while hashing files
  --copy-buffer-size <BYTES>   Copy buffer used for resumable/manual copies
  --max-retries <COUNT>        Maximum attempts per file; omit for unlimited retries
  --retry-delay-ms <MS>        Delay between failed attempts
  --jobs <COUNT>               Worker threads for directory file copies
  --progress                   Print aggregate live progress to stderr
  -v, --verbose                Print a summary after success
  -h, --help                   Show help
  -V, --version                Show version
```

## Examples

Copy one large file with progress enabled:

```powershell
scam.exe + .\big.bin E:\copies\big.bin --progress --verbose
```

Move a whole directory tree using 8 workers:

```powershell
scam.exe - .\photos D:\verified-archive\photos --jobs 8 --verbose
```

Tune both hashing and copy buffers:

```powershell
scam.exe + .\source.dat D:\dest.dat --hash-buffer-size 8388608 --copy-buffer-size 16777216
```

Retry at most 10 times with a short pause between attempts:

```powershell
scam.exe + .\source.dat D:\dest.dat --max-retries 10 --retry-delay-ms 250
```

## Metadata preservation

`scam` preserves metadata captured from the source **before** transfer I/O begins.
That matters for two reasons:

1. a move can safely delete the source after verification while still restoring original metadata on the destination,
2. source access times can change as files are read, but the destination still receives the original captured values.

Currently preserved:

- permissions / read-only bit,
- access time when the platform exposes it,
- modification time when the platform exposes it.

## Resumable transfer behavior

If a destination file already exists, `scam` does not blindly delete it.
Instead it checks whether the existing content can be reused:

- **same size + same checksum** → treat as already copied,
- **smaller size + matching prefix** → resume from the end of that prefix,
- **anything else** → restart from zero.

This makes interrupted copies much cheaper to restart while still keeping end-to-end verification mandatory.

## Parallel directory copying

Single-file transfers remain single-file operations.
Directory transfers, however, can copy multiple files concurrently.
The `--jobs` option controls the number of worker threads used for that stage.

Directory creation and final directory metadata restoration remain ordered and deterministic.

## Project structure

- `src/main.rs` — tiny binary entry point
- `src/lib.rs` — crate entry and public wiring
- `src/cli.rs` — command-line parsing
- `src/model.rs` — shared domain types and execution settings
- `src/plan.rs` — cp/mv-style path resolution and recursive planning
- `src/checksum.rs` — BLAKE3 hashing helpers
- `src/copy.rs` — per-file verified copy, resume, and retry logic
- `src/metadata.rs` — metadata capture and restoration
- `src/progress.rs` — aggregate live progress reporting
- `src/engine.rs` — high-level execution orchestration
- `src/error.rs` — structured error handling
- `tests/integration.rs` — library-level end-to-end tests
- `tests/cli.rs` — CLI-level progress output test

## Build and test

Debug build:

```powershell
cargo build
```

Run the test suite:

```powershell
cargo test
```

Run strict linting:

```powershell
cargo clippy --all-targets -- -D warnings
```

Optimized release build:

```powershell
cargo build --release
```

## Notes on move semantics

A move is implemented as **copy → verify → remove source**.

That means:

- moves work predictably even when a rename fast-path is not available,
- the source is only deleted after the destination verifies,
- directory moves are handled file-by-file, then emptied source directories are removed at the end,
- resumable destinations can still be reused safely before the source is deleted.

This prioritizes data integrity over trying to be a thin wrapper around `rename()`.
