//! Fast checksum generation.
//!
//! `scam` uses BLAKE3 because it is both cryptographically strong and very
//! fast in practice, which matches the tool's goal: verify every copy while
//! still staying as close as possible to the speed of the underlying storage.

use std::{fs::File, io::Read, path::Path};

use blake3::Hash;

use crate::error::{Result, ScamError};

/// Compute the BLAKE3 checksum of a file using the supplied read buffer size.
pub fn checksum_file(path: &Path, buffer_size: usize) -> Result<Hash> {
    let mut file = File::open(path)
        .map_err(|source| ScamError::io("opening file for hashing", path, source))?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0_u8; buffer_size.max(8 * 1024)];

    loop {
        let bytes_read = file
            .read(&mut buffer)
            .map_err(|source| ScamError::io("reading file for hashing", path, source))?;

        if bytes_read == 0 {
            break;
        }

        hasher.update(&buffer[..bytes_read]);
    }

    Ok(hasher.finalize())
}

/// Return a lowercase hexadecimal representation suitable for logging.
pub fn checksum_hex(hash: &Hash) -> String {
    hash.to_hex().to_string()
}

/// Determine whether a file still matches a previously captured checksum.
pub fn file_matches(path: &Path, expected: &Hash, buffer_size: usize) -> Result<bool> {
    Ok(checksum_file(path, buffer_size)? == *expected)
}
