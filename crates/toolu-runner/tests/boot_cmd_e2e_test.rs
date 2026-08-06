//! End-to-end `toolu-runner boot` exit-code tests: a real script step run
//! against a fully-mocked GitHub broker (no mocked business logic — only
//! the GitHub HTTP surface is simulated, matching
//! `listener_smoke_test.rs`'s `run_returns_the_completed_jobs_conclusion`),
//! pinning `boot`'s exit-code mapping through the actual subprocess rather
//! than just the listener's return value:
//! - `boot_exits_zero_on_a_completed_success_job` — Success -> 0.
//! - `boot_exits_one_on_a_failed_job` — Failure -> 1 (a regression flipping
//!   this to 0 would make failed GitHub jobs look green).
//! - `boot_exits_124_when_the_deadline_watchdog_fires_mid_job` — a
//!   `TOOLU_DEADLINE` a couple seconds out fires the watchdog while a
//!   long-running step is in flight, cancels the job gracefully, and the
//!   process exits 124 well before the step would otherwise finish.

use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[path = "helpers/boot_fixtures.rs"]
mod boot_fixtures;

fn temp_home(label: &str) -> std::path::PathBuf {
  let dir = std::env::temp_dir().join(format!(
    "toolu-runner-boot-e2e-test-{label}-{}",
    std::process::id()
  ));
  let _ = std::fs::remove_dir_all(&dir);
  std::fs::create_dir_all(&dir).ok();
  dir
}

#[tokio::test]
async fn boot_exits_zero_on_a_completed_success_job() {
  let server = wiremock::MockServer::start().await;
  boot_fixtures::mount_auth_and_session(&server).await;
  boot_fixtures::mount_job_lifecycle(&server, "true")
    .await
    .expect("mount the broker + run-service mocks");
  let jit_config =
    boot_fixtures::real_jit_config_b64(&server.uri()).expect("build a real-keypair jit config");

  let home = temp_home("success");
  let mut cmd = Command::new(env!("CARGO_BIN_EXE_toolu-runner"));
  let output = cmd
    .arg("boot")
    .env("TOOLU_RUNNER_HOME", &home)
    .env("HOME", &home)
    .env("TOOLU_JITCONFIG", jit_config)
    .env_remove("TOOLU_DEADLINE")
    .output()
    .expect("run binary");

  let stderr = String::from_utf8_lossy(&output.stderr);
  assert_eq!(
    output.status.code(),
    Some(0),
    "expected exit 0 for a completed Success job, stderr: {stderr}"
  );
}

/// Pins the `Conclusion::Failure` -> exit 1 mapping through the real
/// subprocess: same broker lifecycle as the success test, but the script
/// step runs `false` instead of `true`.
#[tokio::test]
async fn boot_exits_one_on_a_failed_job() {
  let server = wiremock::MockServer::start().await;
  boot_fixtures::mount_auth_and_session(&server).await;
  boot_fixtures::mount_job_lifecycle(&server, "false")
    .await
    .expect("mount the broker + run-service mocks");
  let jit_config =
    boot_fixtures::real_jit_config_b64(&server.uri()).expect("build a real-keypair jit config");

  let home = temp_home("failure");
  let mut cmd = Command::new(env!("CARGO_BIN_EXE_toolu-runner"));
  let output = cmd
    .arg("boot")
    .env("TOOLU_RUNNER_HOME", &home)
    .env("HOME", &home)
    .env("TOOLU_JITCONFIG", jit_config)
    .env_remove("TOOLU_DEADLINE")
    .output()
    .expect("run binary");

  let stderr = String::from_utf8_lossy(&output.stderr);
  assert_eq!(
    output.status.code(),
    Some(1),
    "expected exit 1 for a completed Failure job, stderr: {stderr}"
  );
}

/// Pins the deadline-watchdog -> exit 124 mapping through the real
/// subprocess: same broker lifecycle, but the script step `sleep`s far
/// longer than the deadline. `TOOLU_DEADLINE` is set to roughly 2 seconds
/// out (epoch milliseconds, computed from wall-clock `SystemTime`) so the
/// watchdog fires mid-job, gracefully cancels the running step, and the
/// process exits 124 well before the sleep would otherwise finish.
///
/// The mid-job cancel means the job reports `Cancelled` (not `Success`) to
/// the run service — `mount_job_lifecycle`'s `/completejob` mock has no
/// body matcher and no call-count expectation, so it accepts whatever
/// conclusion the cancelled path posts (or no post at all, if the watchdog's
/// hard exit races ahead of it).
#[tokio::test]
async fn boot_exits_124_when_the_deadline_watchdog_fires_mid_job() {
  let server = wiremock::MockServer::start().await;
  boot_fixtures::mount_auth_and_session(&server).await;
  boot_fixtures::mount_job_lifecycle(&server, "sleep 60")
    .await
    .expect("mount the broker + run-service mocks");
  let jit_config =
    boot_fixtures::real_jit_config_b64(&server.uri()).expect("build a real-keypair jit config");

  let now_ms = SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .expect("system clock is after the epoch")
    .as_millis();
  let deadline_ms = now_ms + 2_000;

  let home = temp_home("deadline-watchdog");
  let started = Instant::now();
  let mut cmd = Command::new(env!("CARGO_BIN_EXE_toolu-runner"));
  let output = cmd
    .arg("boot")
    .env("TOOLU_RUNNER_HOME", &home)
    .env("HOME", &home)
    .env("TOOLU_JITCONFIG", jit_config)
    .env("TOOLU_DEADLINE", deadline_ms.to_string())
    .output()
    .expect("run binary");
  let elapsed = started.elapsed();

  let stderr = String::from_utf8_lossy(&output.stderr);
  assert_eq!(
    output.status.code(),
    Some(124),
    "expected exit 124 when the deadline watchdog fires mid-job, stderr: {stderr}"
  );
  // Generous upper bound: the graceful cancel path should land in ~2-5s
  // (deadline + cancel/report round-trip), well under the watchdog's own
  // 30s hard-exit grace period and far short of the 60s sleep it interrupts.
  assert!(
    elapsed < Duration::from_secs(40),
    "watchdog-triggered boot should exit well before the step's sleep finishes, took {elapsed:?}"
  );
}
