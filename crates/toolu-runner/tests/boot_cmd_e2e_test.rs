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
//! - `boot_stands_the_watchdog_down_when_the_job_finishes_first` — the
//!   mirror case: a deadline that never fires must not delay the exit or
//!   change the job's own exit code.

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

/// Run the `boot` subprocess against the mocked broker: one JIT config, an
/// isolated `TOOLU_RUNNER_HOME`, and either an explicit `TOOLU_DEADLINE` or
/// none at all (inherited values are cleared, so a deadline in the test
/// runner's own environment can never leak in).
fn run_boot(
  home: &std::path::Path,
  jit_config: &str,
  deadline_ms: Option<u128>,
) -> std::io::Result<std::process::Output> {
  let mut cmd = Command::new(env!("CARGO_BIN_EXE_toolu-runner"));
  cmd
    .arg("boot")
    .env("TOOLU_RUNNER_HOME", home)
    .env("HOME", home)
    .env("TOOLU_JITCONFIG", jit_config);
  if let Some(ms) = deadline_ms {
    cmd.env("TOOLU_DEADLINE", ms.to_string());
  } else {
    cmd.env_remove("TOOLU_DEADLINE");
  }
  cmd.output()
}

/// Epoch milliseconds `offset` in the future. Saturating, not `expect`: a
/// pre-epoch clock is a host defect, and these tests assert `boot`'s exit
/// codes, not `SystemTime`'s edge cases.
fn deadline_in(offset: Duration) -> u128 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .unwrap_or_default()
    .saturating_add(offset)
    .as_millis()
}

/// `<home>/.toolu-runner/_diag/runner.log` — where `shared::startup` puts the
/// file sink, resolved from `HOME` (`default_data_dir` expands
/// `~/.toolu-runner`), which `run_boot` pins to the test's temp home.
fn read_diag_log(home: &std::path::Path) -> String {
  std::fs::read_to_string(home.join(".toolu-runner/_diag/runner.log")).unwrap_or_default()
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
  let output = run_boot(&home, &jit_config, None).expect("run binary");

  let stderr = String::from_utf8_lossy(&output.stderr);
  assert_eq!(
    output.status.code(),
    Some(0),
    "expected exit 0 for a completed Success job, stderr: {stderr}"
  );
  // `cmd_boot` registers the raw JIT blob with the masker behind the tracing
  // redactor: neither durable sink may carry it verbatim. Deleting that
  // registration is what this assertion is here to catch.
  assert!(
    !read_diag_log(&home).contains(&jit_config),
    "the raw TOOLU_JITCONFIG blob must never reach _diag/runner.log"
  );
  assert!(
    !stderr.contains(&jit_config),
    "the raw TOOLU_JITCONFIG blob must never reach stderr"
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

  let output = run_boot(&temp_home("failure"), &jit_config, None).expect("run binary");

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
/// conclusion the cancelled path posts. The post is asserted, not optional:
/// `cmd_boot` stands the watchdog down once the listener returns, so the
/// hard exit cannot race ahead of the report.
#[tokio::test]
async fn boot_exits_124_when_the_deadline_watchdog_fires_mid_job() {
  let server = wiremock::MockServer::start().await;
  boot_fixtures::mount_auth_and_session(&server).await;
  boot_fixtures::mount_job_lifecycle(&server, "sleep 60")
    .await
    .expect("mount the broker + run-service mocks");
  let jit_config =
    boot_fixtures::real_jit_config_b64(&server.uri()).expect("build a real-keypair jit config");

  let deadline_ms = deadline_in(Duration::from_secs(2));

  let started = Instant::now();
  let output = run_boot(
    &temp_home("deadline-watchdog"),
    &jit_config,
    Some(deadline_ms),
  )
  .expect("run binary");
  let elapsed = started.elapsed();

  let stderr = String::from_utf8_lossy(&output.stderr);
  assert_eq!(
    output.status.code(),
    Some(124),
    "expected exit 124 when the deadline watchdog fires mid-job, stderr: {stderr}"
  );
  // Tight enough to distinguish the two 124 paths: the deadline fires at
  // +2s, so the watchdog's hard exit could not land before +32s (its 30s
  // grace period). Exiting under 25s therefore proves the graceful path
  // won AND that `cmd_boot` stood the watchdog down on its way out,
  // rather than the process being force-killed from the spawned task.
  assert!(
    elapsed < Duration::from_secs(25),
    "the graceful cancel path should exit long before the 30s hard-exit grace, took {elapsed:?}"
  );
  // The graceful path (not the 30s hard exit) must have won: the cancelled
  // job still reports its conclusion to the run service before the process
  // exits — pin that the broker actually saw the /completejob call.
  assert!(
    broker_saw_completejob(&server).await,
    "the watchdog's graceful cancel should report the job via /completejob before exiting"
  );
}

/// Pins the watchdog stand-down: with a deadline far enough out that it can
/// never fire, a job that finishes fast must exit on its own conclusion
/// immediately. Before `cmd_boot` owned the watchdog's `JoinHandle` the task
/// was left sleeping until the deadline, so this is the case that regresses
/// if the `abort()`/join is dropped again.
#[tokio::test]
async fn boot_stands_the_watchdog_down_when_the_job_finishes_first() {
  let server = wiremock::MockServer::start().await;
  boot_fixtures::mount_auth_and_session(&server).await;
  boot_fixtures::mount_job_lifecycle(&server, "true")
    .await
    .expect("mount the broker + run-service mocks");
  let jit_config =
    boot_fixtures::real_jit_config_b64(&server.uri()).expect("build a real-keypair jit config");

  let deadline_ms = deadline_in(Duration::from_secs(3_600));

  let started = Instant::now();
  let output = run_boot(
    &temp_home("watchdog-standdown"),
    &jit_config,
    Some(deadline_ms),
  )
  .expect("run binary");
  let elapsed = started.elapsed();

  let stderr = String::from_utf8_lossy(&output.stderr);
  assert_eq!(
    output.status.code(),
    Some(0),
    "an unfired deadline must not change the job's own exit code, stderr: {stderr}"
  );
  // A watchdog left running would hold the process for the full hour; the
  // 60s bound is orders of magnitude below that and well above a normal run.
  assert!(
    elapsed < Duration::from_secs(60),
    "boot must not wait on the watchdog's sleep after the job completes, took {elapsed:?}"
  );
  assert!(
    !stderr.contains("watchdog task panicked"),
    "aborting the watchdog must not be reported as a panic, stderr: {stderr}"
  );
}

/// Whether the wiremock broker received any `/completejob` call — the
/// cancelled path's proof that the job's conclusion was reported to the run
/// service rather than lost to a hard exit.
async fn broker_saw_completejob(server: &wiremock::MockServer) -> bool {
  server
    .received_requests()
    .await
    .is_some_and(|reqs| reqs.iter().any(|r| r.url.path().ends_with("/completejob")))
}
