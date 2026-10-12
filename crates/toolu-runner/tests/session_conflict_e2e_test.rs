//! Session-create conflicts (#91) through the real `toolu-runner` binary.
//!
//! A wiremock broker replays the live github.com 409 capture
//! (`crates/wire/tests/session_conflict_409.json`: status 409, its headers,
//! empty body) for `POST /session`; the JIT envelope carries a real RSA
//! keypair so every attempt runs the production authenticate → session path.
//! `TOOLU_RUNNER_SESSION_CONFLICT_INTERVAL_MS=50` shrinks upstream's 30 s
//! wait so the 8-attempt window runs in well under a second; the cancel test
//! keeps the default 30 s to prove SIGTERM wakes the wait.
//!
//! - `boot` recovers when the conflict clears inside the window (exit 0).
//! - `boot`, `run --once` and the default `run` loop all exit 5 after exactly
//!   8 session attempts — the loop adds no generic backoff and no re-mint.
//! - SIGTERM during the conflict wait exits 0 promptly.
//! - The conflict is named on stderr; the access token and JIT blob are not.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use config::config::{
  CacheSection, CredentialsFile, RunnerRegistrationConfig, RuntimeConfig, ServicesSection,
  ShadowSection, WorkspaceSection, save_config, save_credentials,
};
use serde_json::Value;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[path = "helpers/boot_fixtures.rs"]
mod boot_fixtures;

type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

const CAPTURE: &str = include_str!("../../wire/tests/session_conflict_409.json");
const INTERVAL_KEY: &str = "TOOLU_RUNNER_SESSION_CONFLICT_INTERVAL_MS";
const CONFLICT_MESSAGE: &str = "another runner session is active for this registration";
/// The access token `boot_fixtures::mount_auth` hands out.
const ACCESS_TOKEN: &str = "fake-token";

/// The captured 409 reply: its status, every recorded header, its empty body.
fn captured_conflict() -> TestResult<ResponseTemplate> {
  let fixture: Value = serde_json::from_str(CAPTURE)?;
  let field = |pointer: &str| {
    fixture
      .pointer(pointer)
      .ok_or(format!("fixture lacks {pointer}"))
  };
  let status = u16::try_from(field("/response/status")?.as_u64().ok_or("status")?)?;
  let mut template = ResponseTemplate::new(status);
  for (name, value) in field("/response/headers")?.as_object().ok_or("headers")? {
    // wiremock computes content-length itself from the (empty) body.
    if name != "content-length" {
      template = template.insert_header(name.as_str(), value.as_str().ok_or("header")?);
    }
  }
  Ok(template.set_body_string(field("/response/body")?.as_str().ok_or("body")?))
}

/// A broker whose `POST /session` always answers with the captured 409.
async fn persistent_conflict_broker() -> TestResult<MockServer> {
  let server = MockServer::start().await;
  boot_fixtures::mount_auth(&server).await;
  Mock::given(method("POST"))
    .and(path("/session"))
    .respond_with(captured_conflict()?)
    .mount(&server)
    .await;
  Ok(server)
}

async fn session_posts(server: &MockServer) -> TestResult<usize> {
  let requests = server
    .received_requests()
    .await
    .ok_or("request recording was disabled")?;
  Ok(
    requests
      .iter()
      .filter(|r| r.method.as_str() == "POST" && r.url.path() == "/session")
      .count(),
  )
}

/// Every file under any `_diag` directory below `home`, concatenated.
fn diag_logs(home: &Path) -> String {
  let mut out = String::new();
  let mut stack = vec![home.to_path_buf()];
  while let Some(dir) = stack.pop() {
    let Ok(entries) = std::fs::read_dir(&dir) else {
      continue;
    };
    for entry in entries.flatten() {
      let path = entry.path();
      if path.is_dir() {
        stack.push(path);
      } else if path.components().any(|c| c.as_os_str() == "_diag") {
        out.push_str(&std::fs::read_to_string(&path).unwrap_or_default());
      }
    }
  }
  out
}

fn boot_cmd(home: &Path, jit_config: &str, interval_ms: Option<&str>) -> Command {
  let mut cmd = Command::new(env!("CARGO_BIN_EXE_toolu-runner"));
  cmd
    .arg("boot")
    .env("HOME", home)
    .env("TOOLU_RUNNER_HOME", home)
    .env("TOOLU_JITCONFIG", jit_config)
    .env_remove("TOOLU_DEADLINE");
  match interval_ms {
    Some(ms) => cmd.env(INTERVAL_KEY, ms),
    None => cmd.env_remove(INTERVAL_KEY),
  };
  cmd
}

/// Persist a registration whose JIT blob points at the wiremock broker.
fn write_registration(home: &Path, jit_config: &str) -> TestResult<PathBuf> {
  let reg_dir = home.join("runners").join("octo").join("demo");
  std::fs::create_dir_all(&reg_dir)?;
  let config_path = reg_dir.join("config.toml");
  let cfg = RunnerRegistrationConfig {
    runner_url: "https://github.com/octo/demo".to_owned(),
    runner_name: "toolu-91-e2e".to_owned(),
    runner_id: 501,
    auth_token: "test-client-id".to_owned(),
    labels: vec!["self-hosted".to_owned()],
    runner_group: "Default".to_owned(),
    ephemeral: false,
    runtime: RuntimeConfig {
      jit_config: jit_config.to_owned(),
      work_dir: reg_dir.join("_work").to_string_lossy().into_owned(),
      data_dir: reg_dir.to_string_lossy().into_owned(),
      protocol_version: "v2".to_owned(),
    },
    services: ServicesSection::default(),
    cache: CacheSection::default(),
    workspace: WorkspaceSection::default(),
    shadow: ShadowSection::default(),
    network: config::network::NetworkSection::default(),
  };
  save_config(&config_path, &cfg)?;
  save_credentials(
    &reg_dir.join("credentials.json"),
    &CredentialsFile {
      access_token: "test-client-id".to_owned(),
      issued_at: "2026-10-11T00:00:00Z".to_owned(),
      expires_at: None,
    },
  )?;
  Ok(config_path)
}

/// `run --config <path>` with no login token anywhere, so a re-mint attempt
/// would be a fatal exit 2 — exit 5 therefore also proves no re-mint ran.
fn run_cmd(home: &Path, config_path: &Path, once: bool) -> Command {
  let mut cmd = Command::new(env!("CARGO_BIN_EXE_toolu-runner"));
  cmd
    .env("HOME", home)
    .env("TOOLU_RUNNER_HOME", home)
    .env("TOOLU_RUNNER_NO_KEYRING", "1")
    .env_remove("TOOLU_RUNNER_TOKEN")
    .env(INTERVAL_KEY, "50")
    .args(["run", "--config"])
    .arg(config_path);
  if once {
    cmd.arg("--once");
  }
  cmd
}

/// The shared exit-5 contract: code, attempt count, conflict named, no
/// credential in stderr or the diag sink.
async fn assert_conflict_exit(
  output: &Output,
  server: &MockServer,
  home: &Path,
  jit_config: &str,
) -> TestResult<()> {
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert_eq!(output.status.code(), Some(5), "stderr: {stderr}");
  assert_eq!(session_posts(server).await?, 8, "stderr: {stderr}");
  assert!(
    stderr.contains("session conflict on attempt 1/8"),
    "{stderr}"
  );
  assert!(stderr.contains(CONFLICT_MESSAGE), "{stderr}");
  let walk_root = home.to_path_buf();
  let diag = tokio::task::spawn_blocking(move || diag_logs(&walk_root)).await?;
  for secret in [ACCESS_TOKEN, jit_config] {
    assert!(!stderr.contains(secret), "credential leaked to stderr");
    assert!(!diag.contains(secret), "credential leaked to the diag log");
  }
  Ok(())
}

/// 91-S1: one captured conflict, then the registration frees up — the job
/// runs to a Success conclusion and `boot` exits 0.
#[tokio::test]
async fn boot_recovers_when_the_conflict_clears_inside_the_window() -> TestResult<()> {
  let server = MockServer::start().await;
  Mock::given(method("POST"))
    .and(path("/session"))
    .respond_with(captured_conflict()?)
    .up_to_n_times(1)
    .with_priority(1)
    .mount(&server)
    .await;
  boot_fixtures::mount_auth_and_session(&server).await;
  boot_fixtures::mount_job_lifecycle(&server, "true").await?;
  let jit_config = boot_fixtures::real_jit_config_b64(&server.uri())?;
  let home = tempfile::tempdir()?;

  let output = boot_cmd(home.path(), &jit_config, Some("50")).output()?;

  let stderr = String::from_utf8_lossy(&output.stderr);
  assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
  assert_eq!(session_posts(&server).await?, 2, "stderr: {stderr}");
  assert!(
    stderr.contains("session conflict on attempt 1/8"),
    "{stderr}"
  );
  Ok(())
}

/// 91-S2: a conflict on every attempt ends `boot` with the distinct code 5.
#[tokio::test]
async fn boot_exits_5_on_a_persistent_conflict() -> TestResult<()> {
  let server = persistent_conflict_broker().await?;
  let jit_config = boot_fixtures::real_jit_config_b64(&server.uri())?;
  let home = tempfile::tempdir()?;

  let output = boot_cmd(home.path(), &jit_config, Some("50")).output()?;

  assert_conflict_exit(&output, &server, home.path(), &jit_config).await
}

/// 91-S3: `--once` keeps the conflict's distinct code (not its generic 2).
#[tokio::test]
async fn run_once_exits_5_on_a_persistent_conflict() -> TestResult<()> {
  let server = persistent_conflict_broker().await?;
  let jit_config = boot_fixtures::real_jit_config_b64(&server.uri())?;
  let home = tempfile::tempdir()?;
  let config_path = write_registration(home.path(), &jit_config)?;

  let output = run_cmd(home.path(), &config_path, true).output()?;

  assert_conflict_exit(&output, &server, home.path(), &jit_config).await
}

/// 91-S3: the always-online loop does not wrap the spent conflict in its
/// generic backoff or a re-mint — exactly 8 attempts, then exit 5.
#[tokio::test]
async fn default_run_loop_exits_5_without_backoff_or_remint() -> TestResult<()> {
  let server = persistent_conflict_broker().await?;
  let jit_config = boot_fixtures::real_jit_config_b64(&server.uri())?;
  let home = tempfile::tempdir()?;
  let config_path = write_registration(home.path(), &jit_config)?;

  let output = run_cmd(home.path(), &config_path, false).output()?;

  assert_conflict_exit(&output, &server, home.path(), &jit_config).await
}

/// 91-S2: SIGTERM during upstream's 30 s conflict wait exits 0 at once.
#[tokio::test]
async fn sigterm_during_the_conflict_wait_exits_zero_promptly() -> TestResult<()> {
  let server = persistent_conflict_broker().await?;
  let jit_config = boot_fixtures::real_jit_config_b64(&server.uri())?;
  let home = tempfile::tempdir()?;
  let mut child = boot_cmd(home.path(), &jit_config, None)
    .stdout(Stdio::null())
    .stderr(Stdio::piped())
    .spawn()?;

  let startup = Instant::now();
  while session_posts(&server).await? == 0 {
    assert!(
      startup.elapsed() < Duration::from_secs(60),
      "boot never attempted a session"
    );
    tokio::time::sleep(Duration::from_millis(50)).await;
  }
  // The first conflict is in; give the WARN + sleep a moment to start.
  tokio::time::sleep(Duration::from_millis(300)).await;
  let signalled = Instant::now();
  let kill = Command::new("kill")
    .args(["-TERM", &child.id().to_string()])
    .status()?;
  assert!(kill.success(), "kill -TERM failed");

  let status = loop {
    if let Some(status) = child.try_wait()? {
      break status;
    }
    assert!(
      signalled.elapsed() < Duration::from_secs(5),
      "boot did not exit promptly after SIGTERM"
    );
    tokio::time::sleep(Duration::from_millis(50)).await;
  };
  let output = child.wait_with_output()?;
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert_eq!(status.code(), Some(0), "stderr: {stderr}");
  assert_eq!(session_posts(&server).await?, 1, "stderr: {stderr}");
  Ok(())
}
