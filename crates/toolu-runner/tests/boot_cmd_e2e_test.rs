//! End-to-end `toolu-runner boot` exit-code test: a real script step run
//! against a fully-mocked GitHub broker (no mocked business logic — only
//! the GitHub HTTP surface is simulated, matching
//! `listener_smoke_test.rs`'s `run_returns_the_completed_jobs_conclusion`),
//! pinning `boot`'s Success -> exit 0 mapping through the actual subprocess
//! rather than just the listener's return value.

use std::process::Command;

#[path = "helpers/boot_fixtures.rs"]
mod boot_fixtures;

fn temp_home() -> std::path::PathBuf {
  let dir = std::env::temp_dir().join(format!("toolu-runner-boot-e2e-test-{}", std::process::id()));
  let _ = std::fs::remove_dir_all(&dir);
  std::fs::create_dir_all(&dir).ok();
  dir
}

#[tokio::test]
async fn boot_exits_zero_on_a_completed_success_job() {
  let server = wiremock::MockServer::start().await;
  boot_fixtures::mount_auth_and_session(&server).await;
  boot_fixtures::mount_job_lifecycle(&server)
    .await
    .expect("mount the broker + run-service mocks");
  let jit_config =
    boot_fixtures::real_jit_config_b64(&server.uri()).expect("build a real-keypair jit config");

  let home = temp_home();
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
