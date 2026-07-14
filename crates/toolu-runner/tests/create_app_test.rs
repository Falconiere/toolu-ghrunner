//! CLI smoke tests for `toolu-runner create-app`.
//!
//! These shell out to the built binary (`CARGO_BIN_EXE_toolu-runner`) and
//! exercise the two guards that must fire BEFORE any socket bind, browser
//! launch, or network call — so every case here exits fast and never hangs
//! waiting on the loopback callback server. `TOOLU_RUNNER_HOME` is pointed
//! at a fresh temp dir per test so nothing touches the real runner home.

use std::process::Command;

/// AC-8: an unsupported `--host` is rejected before any network / browser.
#[test]
fn create_app_rejects_non_github_host() {
  let home = tempfile::tempdir().expect("tempdir");
  let output = Command::new(env!("CARGO_BIN_EXE_toolu-runner"))
    .env("TOOLU_RUNNER_HOME", home.path())
    .args(["create-app", "--host", "ghes.example.com"])
    .output()
    .expect("should run toolu-runner create-app");

  assert!(
    !output.status.success(),
    "create-app against a non-github.com host must fail"
  );
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(
    stderr.contains("is not supported yet") && stderr.contains("github.com only this release"),
    "error should be the host guard naming github.com as the only supported host: {stderr}"
  );
  assert!(
    stderr.contains("ghes.example.com"),
    "error should name the rejected host: {stderr}"
  );
}

/// AC-7: an existing App file blocks creation without `--force`, and the
/// guard fires before the callback server binds — so the process exits
/// immediately AND the existing file is left byte-for-byte untouched.
#[test]
fn create_app_refuses_to_overwrite_without_force() {
  let home = tempfile::tempdir().expect("tempdir");
  let app_path = home.path().join("github-app.json");
  let original = b"not valid json \x00\x01 arbitrary bytes".to_vec();
  std::fs::write(&app_path, &original).expect("write pre-existing app file");

  let output = Command::new(env!("CARGO_BIN_EXE_toolu-runner"))
    .env("TOOLU_RUNNER_HOME", home.path())
    .arg("create-app")
    .output()
    .expect("should run toolu-runner create-app");

  assert!(
    !output.status.success(),
    "create-app must refuse to overwrite an existing App without --force"
  );
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(
    stderr.contains("pass --force to overwrite it"),
    "error should name --force as the override: {stderr}"
  );

  let after = std::fs::read(&app_path).expect("read app file back");
  assert_eq!(
    after, original,
    "the guard must not modify the existing App file"
  );
}

/// `create-app --help` parses and exits cleanly, and documents its flags.
#[test]
fn create_app_help_lists_flags() {
  let output = Command::new(env!("CARGO_BIN_EXE_toolu-runner"))
    .args(["create-app", "--help"])
    .output()
    .expect("should run toolu-runner create-app --help");

  assert!(
    output.status.success(),
    "create-app --help should exit cleanly: {}",
    String::from_utf8_lossy(&output.stderr)
  );
  let stdout = String::from_utf8_lossy(&output.stdout);
  for flag in ["--name <NAME>", "--host <HOST>", "--no-browser", "--force"] {
    assert!(stdout.contains(flag), "missing {flag} in: {stdout}");
  }
}
