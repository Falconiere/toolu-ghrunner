//! Startup check for a Node.js runtime: it must run on this host and report
//! the requested major. Catches a wrong-libc build (e.g. a glibc Node on a
//! musl host), a non-executable or non-Node file, and a mislabeled preseed.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime};

/// Upper bound for `node --version`, generous for a cold disk.
const VERSION_TIMEOUT: Duration = Duration::from_secs(30);

/// Run `<binary> --version` and require exit 0 with output `v{major}.…`.
///
/// # Errors
///
/// Returns a message naming `binary` when it cannot start, times out, exits
/// non-zero, or reports another version.
pub async fn validate_runtime(binary: &Path, major: u8) -> Result<(), String> {
  let fail = |cause: String| {
    format!(
      "Node.js runtime {} failed to start on this host: {cause}",
      binary.display()
    )
  };
  // An unverified binary gets no runner environment (no tokens, no
  // `NODE_OPTIONS` that could alter or break the version check).
  let child = tokio::process::Command::new(binary)
    .arg("--version")
    .env_clear()
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true)
    .output();
  let output = tokio::time::timeout(VERSION_TIMEOUT, child)
    .await
    .map_err(|elapsed| {
      fail(format!(
        "`--version` did not finish within {VERSION_TIMEOUT:?} ({elapsed})"
      ))
    })?
    .map_err(|e| fail(e.to_string()))?;
  if !output.status.success() {
    let stderr = String::from_utf8_lossy(&output.stderr);
    return Err(fail(format!(
      "`--version` exited with {}: {}",
      output.status,
      stderr.trim()
    )));
  }
  let reported = String::from_utf8_lossy(&output.stdout).trim().to_owned();
  let expected = format!("v{major}.");
  if reported.starts_with(&expected) {
    return Ok(());
  }
  Err(format!(
    "Node.js runtime {} reported version {reported:?}, expected v{major}.x",
    binary.display()
  ))
}

/// [`validate_runtime`], skipped for a binary already proven in this process
/// at the same path, major and modification time — a preseeded runtime is
/// otherwise re-run on every pre/main/post stage.
///
/// # Errors
///
/// See [`validate_runtime`].
pub async fn validate_runtime_once(binary: &Path, major: u8) -> Result<(), String> {
  let modified = std::fs::metadata(binary)
    .and_then(|meta| meta.modified())
    .map_err(|e| format!("Node.js runtime {} is unreadable: {e}", binary.display()))?;
  let key = (binary.to_path_buf(), major, modified);
  if with_proven(|set| set.contains(&key)) {
    return Ok(());
  }
  validate_runtime(binary, major).await?;
  with_proven(|set| set.insert(key));
  Ok(())
}

/// Runtimes already proven in this process: (path, major, mtime).
type Proven = HashSet<(PathBuf, u8, SystemTime)>;

/// Run `f` on the proven set under its lock. Synchronous on purpose: the
/// `std` guard never lives across an `.await`.
fn with_proven<T>(f: impl FnOnce(&mut Proven) -> T) -> T {
  static PROVEN: OnceLock<Mutex<Proven>> = OnceLock::new();
  let lock = PROVEN.get_or_init(Mutex::default);
  match lock.lock() {
    Ok(mut set) => f(&mut set),
    Err(poisoned) => f(&mut poisoned.into_inner()),
  }
}

#[cfg(test)]
#[path = "tests/validate.rs"]
mod tests;
