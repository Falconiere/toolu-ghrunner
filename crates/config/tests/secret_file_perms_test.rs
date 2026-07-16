//! Integration tests for the secret-file write hardening (`write_secret_file`
//! via the public `config::save_credentials` chokepoint).
//!
//! Real data only: a real `CredentialsFile` is written into a real `tempfile`
//! dir and its on-disk mode is stat'd back — no mocks. `write_secret_file` is
//! `pub(crate)`, so the tests drive it through `save_credentials` (the
//! credentials.json writer the finding calls out), which funnels through the
//! same chokepoint all four secret writers share.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;

use config::config::{CredentialsFile, save_credentials};

/// The canonical secret payload the tests persist.
fn sample_creds() -> CredentialsFile {
  CredentialsFile {
    access_token: "ghs_phony_test_token".to_owned(),
    issued_at: "2026-07-15T00:00:00Z".to_owned(),
    expires_at: None,
  }
}

/// Finding 1: a target that already exists with a looser mode (0644) is
/// re-tightened to 0600 by the write, not left at its prior mode.
///
/// `.mode(0600)` on `OpenOptions` is honored by the OS only when `O_CREAT`
/// creates the file; the always-online re-mint loop rewrites credentials.json
/// every job, so without the explicit `set_permissions` a pre-existing file
/// keeps whatever mode it had.
#[test]
fn existing_loose_file_is_retightened_to_0600() {
  let dir = tempfile::tempdir().expect("temp dir");
  let path = dir.path().join("credentials.json");

  // Pre-create the target with a world-readable mode.
  fs::write(&path, b"stale contents").expect("pre-create target");
  fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("chmod 0644");
  assert_eq!(
    fs::metadata(&path).expect("stat pre").permissions().mode() & 0o777,
    0o644,
    "precondition: target starts at 0644"
  );

  save_credentials(&path, &sample_creds()).expect("save_credentials");

  let mode = fs::metadata(&path).expect("stat post").permissions().mode() & 0o777;
  assert_eq!(mode, 0o600, "existing file must be re-tightened to 0600");
}

/// Fresh-create path still lands at 0600 (the `.mode()` case is unchanged).
#[test]
fn new_file_is_created_0600() {
  let dir = tempfile::tempdir().expect("temp dir");
  let path = dir.path().join("credentials.json");

  save_credentials(&path, &sample_creds()).expect("save_credentials");

  let mode = fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
  assert_eq!(mode, 0o600, "newly created secret file must be 0600");
}

/// Finding 1 (bonus): a symlink pre-planted at the target is NOT followed —
/// `O_NOFOLLOW` makes the open fail, so the attacker-chosen file it points at
/// is never overwritten with the secret.
#[test]
fn symlink_at_target_is_not_followed() {
  let dir = tempfile::tempdir().expect("temp dir");
  let attacker = dir.path().join("attacker.txt");
  let target = dir.path().join("credentials.json");

  fs::write(&attacker, b"original attacker contents").expect("write attacker file");
  std::os::unix::fs::symlink(&attacker, &target).expect("plant symlink");

  let result = save_credentials(&target, &sample_creds());

  assert!(
    result.is_err(),
    "writing through a symlinked target must fail (O_NOFOLLOW)"
  );
  assert_eq!(
    fs::read(&attacker).expect("read attacker file"),
    b"original attacker contents",
    "the symlink's real target must be untouched by the secret write"
  );
}
