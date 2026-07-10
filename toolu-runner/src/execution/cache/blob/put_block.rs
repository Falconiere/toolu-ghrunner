//! Azure "Put Block": one block of a staged blob, keyed by its (base64) block
//! id. Blocks arrive concurrently and out of order — the Go SDK BuildKit uses
//! stages ~1 MiB blocks — so each is written to a per-upload blocks directory
//! alongside the staging file and assembled later at "Put Block List".

use std::path::{Path, PathBuf};

use axum::body::Bytes;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as SEGMENT;
use shared::RunnerError;

/// Per-upload directory holding staged blocks, a sibling of the staging file.
pub(super) fn blocks_dir(staging: &Path) -> PathBuf {
  let mut name = staging
    .file_name()
    .map(std::ffi::OsStr::to_os_string)
    .unwrap_or_default();
  name.push(".blocks");
  staging.with_file_name(name)
}

/// Filesystem-safe filename for an opaque (base64) block id.
///
/// The client's block id is itself arbitrary base64; re-encoding its bytes with
/// base64url yields a single path segment that can neither traverse nor collide.
pub(super) fn block_filename(block_id: &str) -> String {
  SEGMENT.encode(block_id.as_bytes())
}

/// Store one block's `body` under `staging`'s blocks directory, keyed by id.
///
/// # Errors
/// `RunnerError::Io` if the blocks directory or block file cannot be written.
pub(super) async fn store_block(
  staging: &Path,
  block_id: &str,
  body: Bytes,
) -> Result<(), RunnerError> {
  let dir = blocks_dir(staging);
  tokio::fs::create_dir_all(&dir)
    .await
    .map_err(RunnerError::Io)?;
  let path = dir.join(block_filename(block_id));
  tokio::fs::write(&path, &body)
    .await
    .map_err(RunnerError::Io)?;
  Ok(())
}
