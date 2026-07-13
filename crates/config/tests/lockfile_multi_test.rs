//! Concurrency tests for `config::lockfile` (plan step `locks-concurrency`,
//! spec AC-8).
//!
//! Per-repo runner dirs each get their OWN `.lock`, so two registrations
//! must be able to run at the same time: locks at DIFFERENT paths are
//! independent, while the SAME path stays exclusive. Every test drives the
//! real `fs2` advisory lock against real `tempfile` directories — no mocks.

use config::lockfile::{self, LockBody};
use shared::RunnerError;
use tempfile::TempDir;

/// Lock + config paths for one simulated per-repo runner dir.
fn runner_paths(dir: &TempDir) -> (std::path::PathBuf, std::path::PathBuf) {
  (dir.path().join(".lock"), dir.path().join("config.toml"))
}

/// AC-8: two DIFFERENT lock paths (two per-repo runner dirs) can both be
/// acquired and held simultaneously in one process — per-repo locks do not
/// serialize cross-repo `run`s.
#[test]
fn different_paths_held_simultaneously() {
  let dir_a = TempDir::new().expect("tempdir a");
  let dir_b = TempDir::new().expect("tempdir b");
  let (lock_a, config_a) = runner_paths(&dir_a);
  let (lock_b, config_b) = runner_paths(&dir_b);

  let guard_a = lockfile::acquire(&lock_a, &config_a).expect("acquire runner dir a");
  // Acquired WHILE guard_a is still held — this is the concurrency claim.
  let guard_b =
    lockfile::acquire(&lock_b, &config_b).expect("acquire runner dir b while a is held");

  // Both guards live: each lock file exists and carries ITS OWN body —
  // two distinct locks, not one shared lock observed twice.
  for (lock, config) in [(&lock_a, &config_a), (&lock_b, &config_b)] {
    let raw = std::fs::read_to_string(lock).expect("read lock body");
    let body: LockBody = serde_json::from_str(&raw).expect("parse lock body");
    assert_eq!(body.pid, std::process::id(), "lock body carries our PID");
    assert_eq!(
      body.config_path,
      config.to_string_lossy(),
      "each lock body points at its own runner dir's config"
    );
  }

  drop(guard_a);
  drop(guard_b);
}

/// SAME path while the first guard is held: the second acquire fails with
/// `RunnerError::LockHeld` carrying the live holder's PID (ours).
#[test]
fn same_path_second_acquire_fails_lock_held() {
  let dir = TempDir::new().expect("tempdir");
  let (lock, config) = runner_paths(&dir);

  let _guard = lockfile::acquire(&lock, &config).expect("first acquire");

  let err = lockfile::acquire(&lock, &config).expect_err("second acquire on a held lock must fail");
  assert!(
    matches!(
      &err,
      RunnerError::LockHeld {
        pid,
        started_at,
        config_path,
      } if *pid == std::process::id()
        && !started_at.is_empty()
        && config_path == &config.to_string_lossy()
    ),
    "expected LockHeld with the live holder's PID + config path, got {err:?}"
  );
}

/// Dropping the first guard releases the OS lock: a re-acquire of the SAME
/// path succeeds afterwards.
#[test]
fn drop_releases_and_same_path_reacquires() {
  let dir = TempDir::new().expect("tempdir");
  let (lock, config) = runner_paths(&dir);

  let guard = lockfile::acquire(&lock, &config).expect("first acquire");
  drop(guard);

  // `acquire` retries briefly on the flock release-visibility race, so an
  // immediate re-acquire after drop must succeed.
  let reacquired = lockfile::acquire(&lock, &config).expect("re-acquire after drop");
  drop(reacquired);
}
