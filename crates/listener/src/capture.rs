//! Opt-in capture of acquired job bodies (#103 evidence lane).
//!
//! [`write_acquired`] is always compiled so its tests run in the default
//! gate. Its only production call site, right after `acquire_job` in
//! `job_lifecycle::poll_and_execute`, exists only under the non-default
//! `capture` cargo feature, which release builds never enable. The raw body
//! carries the job's secrets and tokens: the file is created `0600` inside a
//! `0700` directory and must go through `scripts/test/sanitize_capture.py`
//! before it is committed anywhere (`docs/compatibility-evidence.md`).

use std::io::Write;
use std::path::{Path, PathBuf};

/// Write the decoded acquire `body` to
/// `<data_dir>/_diag/captures/<UTC>-<plan_id>.json` and return its path.
///
/// Never fails the job: any I/O or encoding error is WARN-logged and `None`
/// is returned, so a full disk or unwritable directory only loses the
/// capture. The write is synchronous (one small file per acquired job) and
/// only ever runs in a `capture` build, so it is not moved off the async path.
pub fn write_acquired(data_dir: &Path, plan_id: &str, body: &serde_json::Value) -> Option<PathBuf> {
  match try_write(data_dir, plan_id, body) {
    Ok(path) => {
      tracing::info!(path = %path.display(), "captured acquired job body");
      Some(path)
    },
    Err(error) => {
      tracing::warn!(%error, "job body capture failed; continuing without it");
      None
    },
  }
}

fn try_write(data_dir: &Path, plan_id: &str, body: &serde_json::Value) -> std::io::Result<PathBuf> {
  let dir = data_dir.join("_diag").join("captures");
  create_private_dir(&dir)?;
  let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ");
  let name = format!("{stamp}-{}.json", shared::paths::sanitize_job_id(plan_id));
  let path = dir.join(name);
  let bytes = serde_json::to_vec_pretty(body).map_err(std::io::Error::other)?;
  let mut file = open_private(&path)?;
  file.write_all(&bytes)?;
  file.sync_all()?;
  sync_parent(&path)?;
  Ok(path)
}

#[cfg(unix)]
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
  use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
  std::fs::DirBuilder::new()
    .recursive(true)
    .mode(0o700)
    .create(dir)?;
  // A pre-existing directory keeps its old mode; tighten it explicitly.
  std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
  std::fs::create_dir_all(dir)
}

/// Sync the directory entry too, so a crash cannot drop the file name of a
/// capture whose data was already synced.
#[cfg(unix)]
fn sync_parent(path: &Path) -> std::io::Result<()> {
  match path.parent() {
    Some(parent) => std::fs::File::open(parent)?.sync_all(),
    None => Ok(()),
  }
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path) -> std::io::Result<()> {
  Ok(())
}

#[cfg(unix)]
fn open_private(path: &Path) -> std::io::Result<std::fs::File> {
  use std::os::unix::fs::OpenOptionsExt;
  std::fs::OpenOptions::new()
    .write(true)
    .create_new(true)
    .mode(0o600)
    .open(path)
}

#[cfg(not(unix))]
fn open_private(path: &Path) -> std::io::Result<std::fs::File> {
  std::fs::OpenOptions::new()
    .write(true)
    .create_new(true)
    .open(path)
}
