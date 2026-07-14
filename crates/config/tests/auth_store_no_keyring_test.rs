//! Integration tests for `TOOLU_RUNNER_NO_KEYRING` (always-online AC-3).
//!
//! Real data only: tokens are persisted to a real `tempfile` dir and read
//! back — no mocks, no keyring. The pure [`no_keyring_forced`] parse rule is
//! exercised in-process; the end-to-end `AuthStore::new` wiring is proven by
//! re-running this test binary as a subprocess with the env var set, because
//! in-process `std::env::set_var` is `unsafe` under edition 2024 (the
//! workspace denies `unsafe_code`) and would race the parallel test threads.

use std::ffi::OsStr;
use std::path::Path;

use config::auth_store::{self, AuthStore, StoredToken};
use tempfile::TempDir;

/// The canonical token the roundtrip tests persist.
fn sample_token() -> StoredToken {
  StoredToken {
    access_token: "gho_test_no_keyring_token".to_owned(),
    scope: "repo".to_owned(),
    host: "github.com".to_owned(),
    issued_at: "2026-07-14T00:00:00Z".to_owned(),
  }
}

// ── the pure parse rule (set, non-empty, != "0") ────────────────────

#[test]
fn no_keyring_forced_parses_the_env_value() {
  // Not requested: unset, empty, or exactly "0".
  assert!(!auth_store::no_keyring_forced(None));
  assert!(!auth_store::no_keyring_forced(Some(OsStr::new(""))));
  assert!(!auth_store::no_keyring_forced(Some(OsStr::new("0"))));
  // Requested: any other non-empty value.
  assert!(auth_store::no_keyring_forced(Some(OsStr::new("1"))));
  assert!(auth_store::no_keyring_forced(Some(OsStr::new("true"))));
  assert!(auth_store::no_keyring_forced(Some(OsStr::new("yes"))));
  // "00" is not the literal "0", so it still forces the file backend.
  assert!(auth_store::no_keyring_forced(Some(OsStr::new("00"))));
}

// ── the file backend, exercised directly (path + delete) ────────────

#[test]
fn file_backend_roundtrips_and_deletes_at_token_host_json() {
  let dir = TempDir::new().expect("temp dir");
  let store = AuthStore::File(dir.path().to_path_buf());
  let token = sample_token();

  store.save(&token).expect("save token");
  let path = dir.path().join("token-github.com.json");
  assert!(
    path.is_file(),
    "token must persist at token-<host>.json ({})",
    path.display()
  );

  let loaded = store.load("github.com").expect("load token");
  assert_eq!(loaded.map(|t| t.access_token), Some(token.access_token));

  store.delete("github.com").expect("delete token");
  assert!(!path.exists(), "delete must remove the token file");
  assert!(
    store
      .load("github.com")
      .expect("load after delete")
      .is_none(),
    "a deleted token must load as None"
  );
}

// ── end-to-end: TOOLU_RUNNER_NO_KEYRING=1 drives AuthStore::new ──────
//
// WHY subprocess re-exec: `AuthStore::new` reads the process environment,
// and mutating it in-process is off the table (edition-2024 `unsafe`
// `set_var`, denied `unsafe_code`, and a race with parallel threads). This
// re-runs THIS test binary filtered to `helper_no_keyring_roundtrip` with
// the env var set on the child; the helper is a no-op pass in a normal run.

/// Subprocess helper: with `TOOLU_RUNNER_NO_KEYRING=1` set by the parent,
/// build `AuthStore::new` against `$NO_KEYRING_DIR` and prove it selected
/// the file backend by running a save/load/delete cycle.
#[test]
fn helper_no_keyring_roundtrip() {
  if std::env::var_os("NO_KEYRING_HELPER").is_none() {
    return;
  }
  let dir = std::env::var_os("NO_KEYRING_DIR").expect("NO_KEYRING_DIR set by parent");
  let dir = Path::new(&dir);

  let store = AuthStore::new(dir);
  assert!(
    matches!(&store, AuthStore::File(p) if p.as_path() == dir),
    "TOOLU_RUNNER_NO_KEYRING must force the file backend at {}",
    dir.display()
  );

  let token = sample_token();
  store.save(&token).expect("save token");
  let path = dir.join("token-github.com.json");
  assert!(path.is_file(), "token file missing at {}", path.display());

  let loaded = store.load("github.com").expect("load token");
  assert_eq!(loaded.map(|t| t.access_token), Some(token.access_token));

  store.delete("github.com").expect("delete token");
  assert!(!path.exists(), "delete must remove the token file");

  println!("NO_KEYRING_OK {}", path.display());
}

#[test]
fn no_keyring_env_forces_file_backend_end_to_end() {
  let dir = TempDir::new().expect("temp dir");
  let exe = std::env::current_exe().expect("current exe");

  let mut cmd = std::process::Command::new(exe);
  cmd.args(["helper_no_keyring_roundtrip", "--exact", "--nocapture"]);
  cmd.env("NO_KEYRING_HELPER", "1");
  cmd.env("TOOLU_RUNNER_NO_KEYRING", "1");
  cmd.env("NO_KEYRING_DIR", dir.path());
  let out = cmd.output().expect("run helper subprocess");

  let stdout = String::from_utf8_lossy(&out.stdout);
  assert!(
    out.status.success(),
    "helper failed:\n  stdout: {stdout}\n  stderr: {}",
    String::from_utf8_lossy(&out.stderr)
  );
  assert!(
    stdout.contains("NO_KEYRING_OK"),
    "helper must confirm the file-backed roundtrip; stdout: {stdout}"
  );
  assert!(
    stdout.contains("token-github.com.json"),
    "helper must report the expected token file path; stdout: {stdout}"
  );
}
