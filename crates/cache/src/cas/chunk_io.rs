//! Content-addressed file IO: temp + optional fsync + atomic rename,
//! verify-on-read.
//!
//! The atomic rename means a reader never observes a half-written file. It
//! does **not**, by itself, mean the write survives an unclean shutdown:
//! manifest *contents* are fsynced before their rename (see
//! [`Durability::Fsync`]), but nothing in this module ever fsyncs the
//! *parent directory*, so even a fsynced file's directory entry can still
//! vanish after power loss. Directory-entry durability is left to the
//! filesystem.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use shared::RunnerError;

use super::manifest::ChunkId;

/// Write durability for [`write_atomic`].
///
/// `Fsync` restores the historical fsync-before-rename behaviour: the
/// file's *contents* are on stable storage before the rename, at the cost of
/// one blocking `fsync` per call — its directory entry still is not (see the
/// module doc: nothing here fsyncs the parent directory). `Deferred` skips
/// only the `fsync` — create, `write_all`, and the atomic rename are
/// unchanged — trading that durability for throughput; a chunk written this
/// way may exist empty or torn after a crash, but [`verify_bytes`]
/// BLAKE3-checks every chunk on every read, so a torn chunk is never served
/// (see the CAS self-heal path in `store.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Durability {
  /// fsync the file before the atomic rename.
  Fsync,
  /// Skip the fsync; rely on verify-on-read to catch a torn write.
  Deferred,
}

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

/// Write `bytes` to `path` crash-safely (temp + `mode`-gated fsync + atomic
/// rename). Returns `false` (no write) on a dedup hit.
pub(crate) fn write_atomic(
  path: &Path,
  bytes: &[u8],
  mode: Durability,
) -> Result<bool, RunnerError> {
  if path.exists() {
    return Ok(false);
  }
  let parent = path
    .parent()
    .ok_or_else(|| RunnerError::Cache("cas path has no parent".into()))?;
  fs::create_dir_all(parent).map_err(RunnerError::Io)?;
  let tmp = temp_sibling(path);
  write_temp(&tmp, bytes, mode)?;
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

/// Create the temp file and write all bytes; fsync only when `mode` is [`Durability::Fsync`].
fn write_temp(tmp: &Path, bytes: &[u8], mode: Durability) -> Result<(), RunnerError> {
  let mut file = fs::File::create(tmp).map_err(RunnerError::Io)?;
  file.write_all(bytes).map_err(RunnerError::Io)?;
  if matches!(mode, Durability::Fsync) {
    file.sync_all().map_err(RunnerError::Io)?;
  }
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
