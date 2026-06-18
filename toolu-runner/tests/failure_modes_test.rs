//! Failure-mode integration tests for `toolu-runner` (step 8).
//!
//! Each test maps to one of the 12 v1-critical failure paths from the
//! design spec. Tests are real-data: the lock and config tests touch a
//! temp directory; the CLI tests shell out to `cargo run` and exercise
//! the clap surface end-to-end.
//!
//! Failure modes NOT covered here (per spec scope of step 8):
//! - `register` with expired/used token (network call — step 10 live smoke)
//! - `register` with conflicting name (lives in step 10 along with the
//!   actual GH API call)
//! - Disk full mid-job (engine IO path is exercised by the standard
//!   `run_steps` flow; the engine surfaces `RunnerError::Io` which
//!   `run` already logs and treats as a step failure)
//! - Outage > 5 min mid-job (live integration; tracked in
//!   `docs/known-bugs.md`)
//!
//! Each test that shells out to the binary depends on the
//! `toolu-runner` binary having been built with `cargo build` first
//! (or `cargo run`, which builds on demand).

use std::path::PathBuf;
use std::process::Command;

use shared::startup::scan_yamless_env;
use toolu_runner::lockfile::{self, LockBody};

fn toolu_runner() -> Command {
  let mut cmd = Command::new(env!("CARGO"));
  cmd.args(["run", "-p", "toolu-runner", "--quiet", "--"]);
  cmd
}

fn temp_dir(label: &str) -> PathBuf {
  let dir = std::env::temp_dir().join(format!(
    "toolu-runner-failure-modes-{label}-{}",
    std::process::id()
  ));
  let _ = std::fs::remove_dir_all(&dir);
  // `allow-expect-in-tests` only applies inside `#[test]` functions,
  // not helpers — so this helper swallows the create error and the
  // caller is responsible for asserting the dir exists.
  std::fs::create_dir_all(&dir).ok();
  dir
}

// ─── File lock (spec: "`run` started when a previous `run` holds the
//      job lock — exit 2 with PID") ──────────────────────────────────

#[test]
fn lock_acquire_writes_body_and_releases_on_drop() {
  let dir = temp_dir("lock-acquire");
  let lock_path = dir.join(".lock");
  let cfg_path = dir.join("config.toml");

  let guard = lockfile::acquire(&lock_path, &cfg_path).expect("acquire");
  assert!(lock_path.exists(), "lock file should exist after acquire");

  let body: LockBody =
    serde_json::from_str(&std::fs::read_to_string(&lock_path).expect("read lock"))
      .expect("lock body parses");
  assert_eq!(body.pid, std::process::id());
  assert!(!body.started_at.is_empty());
  assert_eq!(body.config_path, cfg_path.to_string_lossy());

  drop(guard);
  // After drop, the lock is released — a fresh acquire should succeed.
  let _second = lockfile::acquire(&lock_path, &cfg_path).expect("reacquire after drop");
}

#[test]
fn lock_conflict_returns_held_pid() {
  let dir = temp_dir("lock-conflict");
  let lock_path = dir.join(".lock");
  let cfg_path = dir.join("config.toml");

  let _guard = lockfile::acquire(&lock_path, &cfg_path).expect("acquire");
  let result = lockfile::acquire(&lock_path, &cfg_path);
  let err = result.expect_err("second acquire must fail with LockHeld");
  let msg = format!("{err}");
  assert!(
    msg.contains(&std::process::id().to_string()),
    "lock-held error should name the holder PID; got: {msg}"
  );
  assert!(msg.contains("started"), "should include started_at: {msg}");
}

#[test]
fn lock_replaces_stale_lock_when_holder_pid_dead() {
  let dir = temp_dir("lock-stale");
  let lock_path = dir.join(".lock");
  let cfg_path = dir.join("config.toml");

  // Write a lock body owned by an obviously-dead PID (PID 0 is reserved
  // by the kernel; nothing runs as PID 0). The mtime is fresh so we
  // also need to backdate it past STALE_LOCK_AGE.
  let stale = LockBody {
    pid: 0,
    started_at: "1970-01-01T00:00:00Z".to_owned(),
    config_path: cfg_path.to_string_lossy().into_owned(),
  };
  std::fs::write(
    &lock_path,
    serde_json::to_string_pretty(&stale).expect("encode"),
  )
  .expect("write stale lock");
  // Backdate mtime past the 5-minute staleness threshold.
  let stale_mtime = filetime::FileTime::from_unix_time(
    chrono::Utc::now().timestamp() - (10 * 60),
    0,
  );
  filetime::set_file_mtime(&lock_path, stale_mtime).expect("backdate");

  // Acquire should detect the stale lock, remove it, and succeed.
  let _guard = lockfile::acquire(&lock_path, &cfg_path).expect("acquire stale lock");
  let body: LockBody =
    serde_json::from_str(&std::fs::read_to_string(&lock_path).expect("read")).expect("parse");
  assert_eq!(body.pid, std::process::id(), "lock body should now be ours");
}

// ─── YAMLESS_* env var warning (spec AC #23) ────────────────────────

#[test]
fn scan_yamless_env_matches_prefix_only() {
  let env = vec![
    ("YAMLESS_API_URL".to_owned(), "x".to_owned()),
    ("YAMLESS_WORKSPACE_ROOT".to_owned(), "x".to_owned()),
    ("PATH".to_owned(), "/usr/bin".to_owned()),
    ("TOOLU_RUNNER_LOG".to_owned(), "info".to_owned()),
    ("YAMLESS_X".to_owned(), "x".to_owned()),
    ("yamless_lower".to_owned(), "x".to_owned()),
  ];
  let keys = scan_yamless_env(env);
  assert_eq!(
    keys,
    vec!["YAMLESS_API_URL".to_owned(), "YAMLESS_WORKSPACE_ROOT".to_owned(), "YAMLESS_X".to_owned()],
    "should return sorted YAMLESS_ keys only"
  );
}

#[test]
fn scan_yamless_env_empty_when_none_set() {
  let env = vec![
    ("PATH".to_owned(), "/usr/bin".to_owned()),
    ("HOME".to_owned(), "/home/me".to_owned()),
  ];
  let keys = scan_yamless_env(env);
  assert!(keys.is_empty(), "got unexpected: {keys:?}");
}

#[test]
fn yamless_warning_emitted_to_stderr_when_env_var_set() {
  // Run the binary with a YAMLESS_* env var set; the warning must
  // appear on stderr. Using a child process avoids touching the
  // host test process's env (which Rust 2024 requires unsafe for).
  let key = "YAMLESS_TEST_WARNING_FROM_CHILD_BAR";
  let value = "ignored-value";
  let output = toolu_runner()
    .args(["status", "--config", "/nonexistent.toml"])
    .env(key, value)
    .output()
    .expect("run binary");
  // `status` exits 2 because the config doesn't exist, but stderr
  // should still contain the YAMLESS warning from `startup::init`.
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(
    stderr.contains(key),
    "expected {key} in stderr warning; got: {stderr}"
  );
  assert!(
    stderr.contains("ignoring yamless env var"),
    "expected warning text; got: {stderr}"
  );
}

#[test]
fn yamless_warning_not_emitted_when_env_var_unset() {
  let output = toolu_runner()
    .args(["status", "--config", "/nonexistent.toml"])
    .env_remove("YAMLESS_API_URL")
    .env_remove("YAMLESS_WORKSPACE_ROOT")
    .env_remove("YAMLESS_CPU_LIMIT")
    .output()
    .expect("run binary");
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(
    !stderr.contains("ignoring yamless env var"),
    "should not emit warning when no YAMLESS_* is set; got: {stderr}"
  );
}

// ─── `run` invoked with no `config.toml` / no `credentials.json` ────

#[test]
fn run_without_config_exits_two_with_pointer() {
  let dir = temp_dir("no-config");
  let cfg = dir.join("config.toml");
  assert!(!cfg.exists());

  let output = toolu_runner()
    .args(["run", "--config"])
    .arg(&cfg)
    .output()
    .expect("run binary");
  assert_eq!(output.status.code(), Some(2), "expected exit 2");
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(
    stderr.contains("config not found"),
    "expected clear pointer in stderr: {stderr}"
  );
  assert!(
    stderr.contains("register"),
    "expected pointer at `register` in: {stderr}"
  );
}

#[test]
fn run_without_credentials_exits_two_with_pointer() {
  let dir = temp_dir("no-creds");
  let cfg = dir.join("config.toml");
  assert!(std::fs::write(&cfg, sample_registration_payload()).is_ok());
  assert!(cfg.exists());
  let creds = dir.join("credentials.json");
  assert!(!creds.exists());

  let output = toolu_runner()
    .args(["run", "--config"])
    .arg(&cfg)
    .output()
    .expect("run binary");
  assert_eq!(output.status.code(), Some(2), "expected exit 2");
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(
    stderr.contains("credentials not found"),
    "expected credentials-not-found pointer in: {stderr}"
  );
}

// ─── `register` URL validation (spec: only github.com / GHES hosts) ─

#[test]
fn register_rejects_non_dot_host() {
  let output = toolu_runner()
    .args([
      "register",
      "--url",
      "https://localhost/owner/repo",
      "--token",
      "fake-token",
    ])
    .output()
    .expect("run binary");
  assert_eq!(output.status.code(), Some(2), "expected exit 2");
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(
    stderr.contains("invalid host") || stderr.contains("invalid --url"),
    "expected URL validation error in: {stderr}"
  );
}

#[test]
fn register_rejects_garbage_url() {
  let output = toolu_runner()
    .args([
      "register",
      "--url",
      "not-a-url-at-all",
      "--token",
      "fake-token",
    ])
    .output()
    .expect("run binary");
  assert_eq!(output.status.code(), Some(2), "expected exit 2");
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(
    stderr.contains("invalid --url") || stderr.contains("invalid host"),
    "expected URL parse error in: {stderr}"
  );
}

// ─── `remove` with no registration (spec: exit 0) ───────────────────

#[test]
fn remove_with_no_registration_exits_zero() {
  let dir = temp_dir("no-registration");
  let cfg = dir.join("config.toml");
  assert!(!cfg.exists());

  let output = toolu_runner()
    .args(["remove", "--config"])
    .arg(&cfg)
    .output()
    .expect("run binary");
  assert_eq!(
    output.status.code(),
    Some(0),
    "remove with no registration should exit 0, got {:?}: {}",
    output.status,
    String::from_utf8_lossy(&output.stderr)
  );
  let stdout = String::from_utf8_lossy(&output.stdout);
  assert!(
    stdout.contains("no registration found"),
    "expected friendly message, got: {stdout}"
  );
}

// ─── Helpers ────────────────────────────────────────────────────────

fn sample_registration_payload() -> String {
  let payload = serde_json::json!({
    "url": "https://github.com/Falconiere/toolu-ghrunner",
    "host": "github.com",
    "name": "test-runner",
    "labels": ["self-hosted"],
    "runner_group": "Default",
    "work": "~/.toolu-runner/_work",
    "data_dir": "~/.toolu-runner",
    "protocol_version": "v2",
  });
  // `allow-expect-in-tests` only applies inside `#[test]` fns —
  // unwrap in a helper triggers `clippy::expect_used`.
  serde_json::to_string_pretty(&payload).unwrap_or_default()
}
