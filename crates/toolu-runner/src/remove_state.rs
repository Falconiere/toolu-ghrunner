//! `remove`'s local state removal (split out of `main.rs`): the
//! `.pending_remove` marker, and deleting a registration's files while
//! keeping `_diag/`. Shared with `ephemeral` (#92).

use std::path::Path;

/// Write the `.pending_remove` marker with owner-only perms (0600 on unix).
///
/// The body is the copied `.lock` JSON (holder PID, `started_at`,
/// `config_path`); it must not be world-readable, matching every other
/// runner state file. `mode(0o600)` only applies when `create` actually
/// creates the file, so an explicit `set_permissions` re-tightens a
/// pre-existing marker left at looser perms.
pub(crate) fn write_pending_marker(path: &Path, body: &str) -> std::io::Result<()> {
  #[cfg(unix)]
  {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let mut f = std::fs::OpenOptions::new()
      .write(true)
      .create(true)
      .truncate(true)
      .mode(0o600)
      .open(path)?;
    f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    f.write_all(body.as_bytes())
  }
  #[cfg(not(unix))]
  {
    std::fs::write(path, body)
  }
}

/// Delete a registration's persisted state: `config.toml`,
/// `credentials.json`, any `.pending_remove` marker, and the `.lock` file
/// (best-effort past the config itself). `_diag/` (logs + job journal) is
/// deliberately kept for `watch` history, and empty parent dirs stay in
/// place.
///
/// `keep_lock` spares `.lock` when a live run still owns it. fs2's advisory
/// lock is inode-scoped and `LockGuard` has no `Drop` that unlinks the path,
/// so removing the file would leave the running process holding a lock on an
/// unlinked inode — a later `run` would create a fresh `.lock`, acquire it,
/// and execute a second job against the same `data_dir` and workspace.
pub(crate) fn delete_registration_state(
  config_path: &Path,
  creds_path: &Path,
  pending: &Path,
  lock_path: &Path,
  keep_lock: bool,
) -> Result<(), std::io::Error> {
  std::fs::remove_file(config_path)?;
  remove_best_effort(creds_path, "credentials");
  remove_best_effort(pending, "pending-remove marker");
  remove_best_effort(
    &pending.with_file_name(::config::stop_marker::FILE_NAME),
    "stop record",
  );
  if !keep_lock {
    remove_best_effort(lock_path, "job lock");
  }
  Ok(())
}

/// Remove `path`, `WARNing` rather than failing if it cannot go.
///
/// These are best-effort past `config.toml` — the registration is already
/// gone once that is deleted — but "best-effort" here means WARN-and-
/// continue, not silence: a leftover `.lock` in particular is what a later
/// `run` weighs in its stale-lock check, so an operator wondering why it
/// reports a job in flight deserves the reason in the log. A missing file
/// is the expected case and stays quiet.
pub(crate) fn remove_best_effort(path: &Path, what: &str) {
  match std::fs::remove_file(path) {
    Ok(()) => {},
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
    Err(e) => tracing::warn!(error = %e, path = %path.display(), "could not remove the {what}"),
  }
}
