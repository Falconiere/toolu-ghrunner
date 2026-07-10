//! Crash-safe content-addressed file IO: temp + fsync + rename, verify-on-read.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use shared::RunnerError;

use super::manifest::ChunkId;

/// Content-addressed path: `<base>/<hex[0..2]>/<hex>`.
pub(crate) fn blob_path(base: &Path, hex: &str) -> PathBuf {
  let shard = hex.get(0..2).unwrap_or("00");
  base.join(shard).join(hex)
}

/// Sibling temp path in the same dir so the final rename is atomic.
fn temp_sibling(path: &Path) -> PathBuf {
  let mut name = path
    .file_name()
    .map(std::ffi::OsStr::to_os_string)
    .unwrap_or_default();
  name.push(".tmp.");
  name.push(uuid::Uuid::new_v4().to_string());
  path.with_file_name(name)
}

/// Write `bytes` to `path` crash-safely. Returns `false` (no write) on a dedup hit.
pub(crate) fn write_atomic_sync(path: &Path, bytes: &[u8]) -> Result<bool, RunnerError> {
  if path.exists() {
    return Ok(false);
  }
  let parent = path
    .parent()
    .ok_or_else(|| RunnerError::Cache("cas path has no parent".into()))?;
  fs::create_dir_all(parent).map_err(RunnerError::Io)?;
  let tmp = temp_sibling(path);
  write_and_sync(&tmp, bytes)?;
  match fs::rename(&tmp, path) {
    Ok(()) => Ok(true),
    Err(e) => {
      let _ = fs::remove_file(&tmp);
      if path.exists() {
        Ok(false)
      } else {
        Err(RunnerError::Io(e))
      }
    },
  }
}

/// Create the temp file, write all bytes, and fsync before it is renamed.
fn write_and_sync(tmp: &Path, bytes: &[u8]) -> Result<(), RunnerError> {
  let mut file = fs::File::create(tmp).map_err(RunnerError::Io)?;
  file.write_all(bytes).map_err(RunnerError::Io)?;
  file.sync_all().map_err(RunnerError::Io)?;
  Ok(())
}

/// Read a content-addressed file, re-hashing it; a BLAKE3 mismatch is an error, never served.
pub(crate) async fn read_verified(path: &Path, id: &ChunkId) -> Result<Vec<u8>, RunnerError> {
  let bytes = tokio::fs::read(path).await.map_err(RunnerError::Io)?;
  verify_bytes(&bytes, id)?;
  Ok(bytes)
}

/// BLAKE3-verify `bytes` against `id`; a mismatch is an error, never served.
///
/// # Errors
/// `RunnerError::Cache` if the digest of `bytes` does not equal `id`.
pub(crate) fn verify_bytes(bytes: &[u8], id: &ChunkId) -> Result<(), RunnerError> {
  if blake3::hash(bytes).as_bytes() == &id.0 {
    Ok(())
  } else {
    Err(RunnerError::Cache(format!(
      "chunk digest mismatch: expected {}",
      id.to_hex()
    )))
  }
}
