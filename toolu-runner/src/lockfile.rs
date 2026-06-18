//! Single-job file lock for `toolu-runner run`.
//!
//! Two `toolu-runner run` processes cannot share a registration — GH
//! assigns a job to one runner and the duplicate would race on the
//! session/broker. The lock guarantees at most one live `run` per
//! data directory at any time.
//!
//! ## Wire format
//!
//! The lock file lives at `<data_dir>/.lock`. Its body is JSON:
//!
//! ```json
//! {"pid": 12345, "started_at": "2026-06-18T10:00:00Z", "config_path": "/Users/foo/.toolu-runner/config.toml"}
//! ```
//!
//! ## Locking primitive
//!
//! Uses `fs2::FileExt::lock_exclusive` (advisory, OS-enforced) on the
//! underlying file descriptor. The lock is held for the lifetime of the
//! returned [`LockGuard`]; dropping the guard releases it (even on panic,
//! via `Drop`).
//!
//! ## Stale lock detection
//!
//! A second `run` that finds the lock held reads the body and checks:
//! 1. is the holder PID still alive? If yes → exit 2 with the PID message.
//! 2. is the lock older than 5 minutes AND the holder is dead? → remove
//!    the stale lock and try again.
//!
//! Watchdog is intentionally simple — the runner doesn't fork a watcher
//! task. The second `run` does the cleanup when it tries to acquire.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use shared::RunnerError;
use sysinfo::{Pid, ProcessesToUpdate, System};

/// Default staleness threshold: if the lock body is older than this AND
/// the holder PID is not alive, a new `run` may remove the lock and try
/// again. Matches the spec's "watchdog timer" semantics.
pub const STALE_LOCK_AGE: Duration = Duration::from_secs(5 * 60);

/// JSON body of the `.lock` file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockBody {
  /// Process ID of the holder. Detected as stale by `sysinfo`.
  pub pid: u32,
  /// RFC3339 timestamp the holder wrote the lock at.
  pub started_at: String,
  /// Path to the `config.toml` the holder is using.
  pub config_path: String,
}

/// Handle returned by [`acquire`]. Holds the lock file exclusively until
/// dropped; releasing the lock is automatic.
pub struct LockGuard {
  /// Kept alive so the OS advisory lock on the file descriptor stays
  /// held. `Drop` does not need to read it — dropping the `File` is
  /// what releases the lock. Underscore-prefixed so the compiler sees
  /// the field as intentionally retained-for-Drop, not dead code.
  _file: std::fs::File,
  /// Cached for [`Debug`] and for stale-lock detection on retry.
  path: PathBuf,
}

impl std::fmt::Debug for LockGuard {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("LockGuard")
      .field("path", &self.path)
      .finish()
  }
}

/// Acquire the single-job lock at `path`, creating the parent directory
/// if needed.
///
/// Behavior:
/// 1. Open (or create) the lock file.
/// 2. Try `lock_exclusive` (non-blocking).
/// 3. If contended, read the body:
///    - holder PID alive → return `Err(RunnerError::LockHeld { pid, .. })`.
///    - holder PID dead AND lock older than [`STALE_LOCK_AGE`] → remove
///      the lock and retry acquisition once.
///    - holder PID dead but lock fresh → treat as a concurrent acquire in
///      flight; return `Err(LockHeld)` with the stale PID rather than
///      racing the kernel lock release.
///
/// On success, writes a fresh [`LockBody`] (with the current PID and
/// `started_at`) to the file and returns a [`LockGuard`].
///
/// # Errors
///
/// Returns `RunnerError::LockHeld` if another live `run` holds the lock.
/// Returns `RunnerError::Io` on filesystem failures (parent dir creation,
/// file open, lock acquisition).
pub fn acquire(path: &Path, config_path: &Path) -> Result<LockGuard, RunnerError> {
  if let Some(parent) = path.parent() {
    std::fs::create_dir_all(parent)?;
  }

  let body = LockBody {
    pid: std::process::id(),
    started_at: chrono::Utc::now().to_rfc3339(),
    config_path: config_path.to_string_lossy().into_owned(),
  };

  let file = OpenOptions::new()
    .create(true)
    .write(true)
    .truncate(false)
    .open(path)?;

  match file.try_lock_exclusive() {
    Ok(()) => {
      write_body(&file, &body)?;
      Ok(LockGuard {
        _file: file,
        path: path.to_path_buf(),
      })
    },
    Err(_) => handle_contended(path, &body),
  }
}

fn handle_contended(path: &Path, body: &LockBody) -> Result<LockGuard, RunnerError> {
  // Try to read what the holder wrote. If the body is malformed we treat
  // it as a held lock — better to fail closed than silently steal a lock
  // the kernel says is held.
  let existing = read_body(path).ok();
  let now = SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .unwrap_or_default();
  let mtime = std::fs::metadata(path)
    .and_then(|m| m.modified())
    .ok()
    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
    .unwrap_or_default();
  let age = now.saturating_sub(mtime);

  if let Some(existing) = existing
    && (is_pid_alive(existing.pid) || age < STALE_LOCK_AGE)
  {
    return Err(RunnerError::LockHeld {
      pid: existing.pid,
      started_at: existing.started_at,
      config_path: existing.config_path,
    });
  }

  // Stale lock — remove and retry once.
  let _ = std::fs::remove_file(path);
  let file = OpenOptions::new()
    .create(true)
    .write(true)
    .truncate(false)
    .open(path)?;
  file
    .lock_exclusive()
    .map_err(|e| RunnerError::Config(format!("lock acquire after stale-lock removal: {e}")))?;
  write_body(&file, body)?;
  Ok(LockGuard {
    _file: file,
    path: path.to_path_buf(),
  })
}

fn write_body(file: &std::fs::File, body: &LockBody) -> Result<(), RunnerError> {
  let mut f = file;
  f.set_len(0)?;
  let json = serde_json::to_string_pretty(body)?;
  f.write_all(json.as_bytes())?;
  f.sync_all()?;
  Ok(())
}

fn read_body(path: &Path) -> Result<LockBody, RunnerError> {
  let raw = std::fs::read_to_string(path)?;
  serde_json::from_str(&raw).map_err(|e| RunnerError::Config(format!("lock body parse: {e}")))
}

/// Is `pid` still alive? Uses `sysinfo::System` to check the local
/// process table. Returns `false` for `0` and unknown PIDs.
pub fn is_pid_alive(pid: u32) -> bool {
  if pid == 0 {
    return false;
  }
  let mut sys = System::new();
  sys.refresh_processes(ProcessesToUpdate::All, true);
  sys.process(Pid::from_u32(pid)).is_some()
}
