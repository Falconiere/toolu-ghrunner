//! Shell-out tests for `toolu-runner boot` (container image zero-config
//! one-shot mode). Invokes the compiled bin directly via `CARGO_BIN_EXE_`
//! (same debug binary — `debug_assert_cli` still runs at startup) rather
//! than `cli_test.rs`'s `cargo run` shim: the past-deadline test asserts
//! wall-clock, and a `cargo run` shell-out can stall for minutes on the
//! target-dir lock while parallel tests build.

use std::process::Command;
use std::time::{Duration, Instant};

fn toolu_runner() -> Command {
  Command::new(env!("CARGO_BIN_EXE_toolu-runner"))
}

fn temp_home(label: &str) -> std::path::PathBuf {
  let dir = std::env::temp_dir().join(format!(
    "toolu-runner-boot-test-{label}-{}",
    std::process::id()
  ));
  let _ = std::fs::remove_dir_all(&dir);
  std::fs::create_dir_all(&dir).ok();
  dir
}

/// A real-shaped, parseable 3-blob JIT envelope. Its `ServerUrlV2` points
/// at `vstst.action.github.com`, so it is safe for the past-deadline test
/// (no network is ever attempted) but not for a poll — that needs a
/// wiremock broker (see `boot_cmd_e2e_test.rs`).
///
/// Returns `Result` (not `Option`/panic) so this non-`#[test]` helper stays
/// clippy-clean — `allow-expect-in-tests` only covers `#[test]` fns, so the
/// caller `.expect()`s the result instead.
fn fixture_jit_config_b64() -> Result<String, String> {
  const FIXTURE: &str = include_str!("fixtures/jit_config_github_com.json");
  let v: serde_json::Value =
    serde_json::from_str(FIXTURE).map_err(|e| format!("parse fixture json: {e}"))?;
  v.get("encoded")
    .and_then(serde_json::Value::as_str)
    .map(str::to_owned)
    .ok_or_else(|| "fixture has no `encoded` field".to_owned())
}

#[test]
fn boot_without_jitconfig_exits_two_naming_the_var() {
  let home = temp_home("no-jitconfig");
  let output = toolu_runner()
    .arg("boot")
    .env("TOOLU_RUNNER_HOME", &home)
    .env("HOME", &home)
    .env_remove("TOOLU_JITCONFIG")
    .env_remove("TOOLU_DEADLINE")
    .output()
    .expect("run binary");

  assert_eq!(output.status.code(), Some(2), "expected exit 2");
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(
    stderr.contains("TOOLU_JITCONFIG is not set (or empty)"),
    "stderr should carry the full missing-TOOLU_JITCONFIG message: {stderr}"
  );
}

/// A garbage, non-decimal `TOOLU_DEADLINE` must not itself be fatal — the
/// missing-jitconfig check still wins and still exits 2 naming the right
/// variable, proving the deadline parse failure only WARNs and falls
/// through rather than erroring out on its own.
#[test]
fn boot_with_unparseable_deadline_and_no_jitconfig_still_exits_two() {
  let home = temp_home("bad-deadline-no-jitconfig");
  let output = toolu_runner()
    .arg("boot")
    .env("TOOLU_RUNNER_HOME", &home)
    .env("HOME", &home)
    .env_remove("TOOLU_JITCONFIG")
    .env("TOOLU_DEADLINE", "not-a-number")
    .output()
    .expect("run binary");

  assert_eq!(output.status.code(), Some(2), "expected exit 2");
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(
    stderr.contains("TOOLU_JITCONFIG is not set (or empty)"),
    "stderr should still carry the full missing-TOOLU_JITCONFIG message: {stderr}"
  );
}

#[test]
fn boot_with_past_deadline_exits_124_fast_with_no_network() {
  let home = temp_home("past-deadline");
  let started = Instant::now();
  let output = toolu_runner()
    .arg("boot")
    .env("TOOLU_RUNNER_HOME", &home)
    .env("HOME", &home)
    .env(
      "TOOLU_JITCONFIG",
      fixture_jit_config_b64().expect("load fixture jit config"),
    )
    .env("TOOLU_DEADLINE", "1") // 1 ms after the epoch: always past.
    .output()
    .expect("run binary");
  let elapsed = started.elapsed();

  assert_eq!(output.status.code(), Some(124), "expected exit 124");
  // Generous bound: no network call happens on this path, so the only
  // real cost is process/tracing startup.
  assert!(
    elapsed < Duration::from_secs(30),
    "past-deadline boot should exit fast, took {elapsed:?}"
  );
}

#[test]
fn boot_help_documents_both_env_vars() {
  let output = toolu_runner()
    .args(["boot", "--help"])
    .output()
    .expect("run binary");

  assert!(output.status.success(), "boot --help should exit cleanly");
  let stdout = String::from_utf8_lossy(&output.stdout);
  assert!(
    stdout.contains("TOOLU_JITCONFIG"),
    "missing TOOLU_JITCONFIG in: {stdout}"
  );
  assert!(
    stdout.contains("TOOLU_DEADLINE"),
    "missing TOOLU_DEADLINE in: {stdout}"
  );
}

#[test]
fn top_level_help_lists_boot() {
  let output = toolu_runner().arg("--help").output().expect("run binary");

  assert!(output.status.success(), "--help should exit cleanly");
  let stdout = String::from_utf8_lossy(&output.stdout);
  assert!(stdout.contains("boot"), "missing boot in: {stdout}");
}
