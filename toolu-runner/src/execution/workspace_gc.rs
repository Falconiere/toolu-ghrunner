//! Age-based garbage collection of per-job workspace directories.
//!
//! `job_runner::prepare_job_dirs` creates `workspace_root/<job_id>` per job and
//! never deletes it; on a long-lived runner these accumulate without bound.
//! `gc_workspaces` prunes the stale ones (mtime older than a configured age) at
//! the start of each job, always sparing the currently-running job's directory.

use std::fs::{self, DirEntry, Metadata};
use std::path::Path;
use std::time::{Duration, SystemTime};

use shared::RunnerError;

/// Remove immediate child directories of `workspace_root` whose mtime is older
/// than `max_age`, except the one named `keep` (the currently-running job).
/// Returns how many were removed.
///
/// Best-effort per entry: an entry that cannot be stat'd or removed is logged
/// (WARN) and skipped, never aborting the sweep. A missing `workspace_root` is
/// `Ok(0)`. Only directories are considered — plain files are left untouched.
///
/// # Errors
///
/// Returns `RunnerError::Io` if `workspace_root` exists but its directory
/// listing cannot be read.
pub fn gc_workspaces(
  workspace_root: &Path,
  max_age: Duration,
  keep: &str,
) -> Result<usize, RunnerError> {
  let entries = match fs::read_dir(workspace_root) {
    Ok(entries) => entries,
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
    Err(e) => return Err(e.into()),
  };

  let now = SystemTime::now();
  let mut removed = 0usize;
  for entry in entries {
    match entry {
      Ok(entry) if prune_entry(&entry, now, max_age, keep) => {
        removed += 1;
      },
      Ok(_) => {},
      Err(e) => {
        tracing::warn!(error = %e, "workspace GC: skipping unreadable directory entry");
      },
    }
  }
  Ok(removed)
}

/// Remove a single entry when it is a stale job directory. Returns `true` only
/// when it was removed. All I/O failures are logged (WARN) and treated as "not
/// removed" so the sweep continues.
fn prune_entry(entry: &DirEntry, now: SystemTime, max_age: Duration, keep: &str) -> bool {
  is_stale_job_dir(entry, now, max_age, keep) && remove_workspace(&entry.path())
}

/// Whether `entry` is a stale per-job workspace eligible for pruning: a
/// directory, not `keep`, with an mtime older than `max_age`. An entry that
/// cannot be stat'd is logged (WARN) and treated as ineligible.
fn is_stale_job_dir(entry: &DirEntry, now: SystemTime, max_age: Duration, keep: &str) -> bool {
  if entry.file_name().as_os_str() == keep {
    return false;
  }
  match entry.metadata() {
    Ok(metadata) => metadata.is_dir() && is_older_than(&entry.path(), &metadata, now, max_age),
    Err(e) => {
      tracing::warn!(path = %entry.path().display(), error = %e, "workspace GC: cannot stat entry, skipping");
      false
    },
  }
}

/// Recursively remove a stale workspace directory, logging success (DEBUG) or
/// failure (WARN). Returns `true` only when the directory was removed.
fn remove_workspace(path: &Path) -> bool {
  match fs::remove_dir_all(path) {
    Ok(()) => {
      tracing::debug!(path = %path.display(), "workspace GC: removed stale job workspace");
      true
    },
    Err(e) => {
      tracing::warn!(path = %path.display(), error = %e, "workspace GC: cannot remove entry, skipping");
      false
    },
  }
}

/// Whether `metadata`'s mtime is more than `max_age` before `now`.
///
/// A mtime at or after `now` — clock skew, or `duration_since` failing on a
/// future-dated entry — is treated as fresh, so such a directory is never
/// pruned. An unreadable mtime (unsupported platform) is likewise treated as
/// fresh (never delete when unsure) and logged at DEBUG.
fn is_older_than(path: &Path, metadata: &Metadata, now: SystemTime, max_age: Duration) -> bool {
  let Ok(mtime) = metadata.modified() else {
    tracing::debug!(path = %path.display(), "workspace GC: mtime unavailable, treating entry as fresh");
    return false;
  };
  match now.duration_since(mtime) {
    Ok(age) => age > max_age,
    Err(_) => false,
  }
}
