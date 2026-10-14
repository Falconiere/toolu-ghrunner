//! `run` on an `--ephemeral` registration (#92) through the real binary.
//!
//! The persisted registration carries a JIT envelope with a real RSA keypair
//! whose broker, and the registration URL's runners API, point at one local
//! HTTP endpoint. The acquired job is the captured `fixtures/job_message.json`
//! cut to one real shell step, so each run executes a real process and
//! reports a real conclusion. No `--once` is passed: the ephemeral mode
//! itself must stop after one job, never re-mint, unregister, and clean up.
//!
//! - AC-5: success, failure and SIGTERM-stopped jobs → one acquire, no
//!   `generate-jitconfig`, one DELETE, local state removed, exit 0.
//! - AC-6: SIGTERM before any job keeps everything; a restarted `run` on the
//!   same registration then runs its job and cleans up.
//! - AC-7: DELETE 500 keeps state and exits 2 naming `toolu-runner remove`;
//!   404 + an empty runner list counts as removed; no token → removed,
//!   "not verified".
//! - Loop edges: org URL (unverified removal), a transient failure retried
//!   on the same registration, `--once`, `.pending_remove`, a session
//!   conflict (exit 5), and a completion failure after the acquire (cleaned,
//!   exit 1).
//! - AC-12: a runner deleted on GitHub (the live 400 `invalid_client`
//!   token-exchange capture) exits 1 and keeps state.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use config::config::{
  CacheSection, CredentialsFile, RunnerRegistrationConfig, RuntimeConfig, ServicesSection,
  ShadowSection, WorkspaceSection, save_config, save_credentials,
};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::boot_fixtures;

type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

/// The persisted runner id — the one the DELETE must name.
const RUNNER_ID: i64 = 501;
const RUNNER_NAME: &str = "toolu-92-e2e";
const RUNNERS_PATH: &str = "/api/v3/repos/octo/demo/actions/runners";
const TOKEN: &str = "ephemeral-92-token";
/// Budget for one `run` to finish on its own.
const EXIT_BUDGET: Duration = Duration::from_secs(90);
/// A file in the registration's `_diag/` that cleanup must leave alone.
const DIAG_MARKER: &str = "history.log";

/// GitHub's live reply to the token exchange of a deleted JIT registration.
const INVALID_CLIENT: &str = include_str!("../../wire/tests/oauth_invalid_client_400.json");

/// The captured `invalid_client` reply: its status, content type and body.
fn invalid_client_capture() -> TestResult<ResponseTemplate> {
  let capture: Value = serde_json::from_str(INVALID_CLIENT)?;
  let status = capture
    .pointer("/response/status")
    .and_then(Value::as_u64)
    .ok_or("status")?;
  let body = capture
    .pointer("/response/body")
    .and_then(Value::as_str)
    .ok_or("body")?;
  Ok(
    ResponseTemplate::new(u16::try_from(status)?)
      .insert_header("content-type", "application/json; charset=utf-8")
      .set_body_string(body),
  )
}

fn delete_path() -> String {
  format!("{RUNNERS_PATH}/{RUNNER_ID}")
}

/// Persist an ephemeral registration pointing at `server`.
fn write_registration(home: &Path, server: &MockServer) -> TestResult<PathBuf> {
  write_registration_at(home, server, "octo/demo")
}

/// Persist an ephemeral registration for `<server>/<url_path>`.
fn write_registration_at(home: &Path, server: &MockServer, url_path: &str) -> TestResult<PathBuf> {
  let jit = boot_fixtures::real_jit_config_b64(&server.uri())?;
  let reg_dir = home.join("runners").join("octo").join("demo");
  std::fs::create_dir_all(reg_dir.join("_diag"))?;
  std::fs::write(reg_dir.join("_diag").join(DIAG_MARKER), "kept")?;
  let config_path = reg_dir.join("config.toml");
  let cfg = RunnerRegistrationConfig {
    runner_url: format!("{}/{url_path}", server.uri()),
    runner_name: RUNNER_NAME.to_owned(),
    runner_id: RUNNER_ID,
    auth_token: "test-client-id".to_owned(),
    labels: vec!["self-hosted".to_owned()],
    runner_group: "Default".to_owned(),
    ephemeral: true,
    runtime: RuntimeConfig {
      jit_config: jit,
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

/// Mount the runners API: the DELETE answer, an empty name lookup, and a
/// `generate-jitconfig` that must never be called.
async fn mount_runners_api(server: &MockServer, delete_status: u16) {
  Mock::given(method("DELETE"))
    .and(path(delete_path()))
    .respond_with(ResponseTemplate::new(delete_status))
    .mount(server)
    .await;
  Mock::given(method("GET"))
    .and(path(RUNNERS_PATH))
    .respond_with(
      ResponseTemplate::new(200).set_body_json(json!({"total_count": 0, "runners": []})),
    )
    .mount(server)
    .await;
  Mock::given(method("POST"))
    .and(path(format!("{RUNNERS_PATH}/generate-jitconfig")))
    .respond_with(ResponseTemplate::new(500))
    .mount(server)
    .await;
}

/// A broker that serves one job running `script`, plus the runners API.
async fn job_server(script: &str, delete_status: u16) -> TestResult<MockServer> {
  let server = MockServer::start().await;
  boot_fixtures::mount_auth_and_session(&server).await;
  boot_fixtures::mount_job_lifecycle(&server, script).await?;
  mount_runners_api(&server, delete_status).await;
  Ok(server)
}

/// A broker with a live session but no job (long-poll 202s).
async fn idle_server() -> TestResult<MockServer> {
  let server = MockServer::start().await;
  boot_fixtures::mount_auth_and_session(&server).await;
  Mock::given(method("GET"))
    .and(path("/message"))
    .respond_with(ResponseTemplate::new(202).set_delay(Duration::from_millis(200)))
    .mount(&server)
    .await;
  mount_runners_api(&server, 204).await;
  Ok(server)
}

/// Spawn `run --config <path>` (no `--once`), with or without a token.
fn spawn_run(home: &Path, config_path: &Path, token: Option<&str>) -> TestResult<Child> {
  spawn_run_with(home, config_path, token, &[])
}

/// [`spawn_run`] with extra `run` arguments (e.g. `--once`). The session
/// conflict wait is shrunk to 50 ms so an 8-attempt conflict ends fast.
fn spawn_run_with(
  home: &Path,
  config_path: &Path,
  token: Option<&str>,
  extra: &[&str],
) -> TestResult<Child> {
  let mut cmd = Command::new(env!("CARGO_BIN_EXE_toolu-runner"));
  cmd
    .env("HOME", home)
    .env("TOOLU_RUNNER_HOME", home)
    .env("TOOLU_RUNNER_NO_KEYRING", "1")
    .env_remove("TOOLU_RUNNER_TOKEN")
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .env("TOOLU_RUNNER_SESSION_CONFLICT_INTERVAL_MS", "50")
    .args(["run", "--config"])
    .arg(config_path)
    .args(extra);
  if let Some(token) = token {
    cmd.env("TOOLU_RUNNER_TOKEN", token);
  }
  Ok(cmd.spawn()?)
}

/// Wait for `child` to exit on its own within [`EXIT_BUDGET`].
async fn wait_exit(mut child: Child) -> TestResult<Output> {
  let deadline = Instant::now() + EXIT_BUDGET;
  while child.try_wait()?.is_none() {
    if Instant::now() > deadline {
      child.kill()?;
      return Err("run did not exit within the budget".into());
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
  }
  Ok(child.wait_with_output()?)
}

/// Requests received for `method_name path_name`.
async fn count(server: &MockServer, method_name: &str, path_name: &str) -> TestResult<usize> {
  let requests = server
    .received_requests()
    .await
    .ok_or("recording disabled")?;
  Ok(
    requests
      .iter()
      .filter(|r| r.method.as_str() == method_name && r.url.path() == path_name)
      .count(),
  )
}

/// Wait until `server` has seen `method_name path_name` at least once.
async fn wait_for(server: &MockServer, method_name: &str, path_name: &str) -> TestResult<()> {
  let deadline = Instant::now() + EXIT_BUDGET;
  while count(server, method_name, path_name).await? == 0 {
    if Instant::now() > deadline {
      return Err(format!("never saw {method_name} {path_name}").into());
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
  }
  Ok(())
}

fn sigterm(child: &Child) -> TestResult<()> {
  let status = Command::new("kill")
    .args(["-TERM", &child.id().to_string()])
    .status()?;
  if status.success() {
    Ok(())
  } else {
    Err("kill -TERM failed".into())
  }
}

/// The `conclusion` the runner reported in its single `completejob`.
async fn reported_conclusion(server: &MockServer) -> TestResult<String> {
  let requests = server
    .received_requests()
    .await
    .ok_or("recording disabled")?;
  let complete = requests
    .iter()
    .find(|r| r.url.path() == "/completejob")
    .ok_or("no completejob request")?;
  let body: Value = serde_json::from_slice(&complete.body)?;
  let conclusion = body.get("conclusion").and_then(Value::as_str);
  Ok(conclusion.ok_or("conclusion")?.to_owned())
}

fn text(bytes: &[u8]) -> String {
  String::from_utf8_lossy(bytes).into_owned()
}

/// Assert the registration's local state is (or is not) still present;
/// `_diag/` always survives.
fn assert_state(config_path: &Path, present: bool, out: &Output) {
  let dir = config_path.parent().unwrap_or(config_path);
  let ctx = format!(
    "stdout: {}\nstderr: {}",
    text(&out.stdout),
    text(&out.stderr)
  );
  assert_eq!(config_path.exists(), present, "config.toml; {ctx}");
  assert_eq!(
    dir.join("credentials.json").exists(),
    present,
    "credentials; {ctx}"
  );
  assert!(
    dir.join("_diag").join(DIAG_MARKER).exists(),
    "_diag is kept; {ctx}"
  );
}

/// AC-5 shared contract after a job: exit 0, one acquire, no re-mint, one
/// DELETE, state gone, the reported conclusion.
async fn assert_ran_once_and_cleaned(
  out: &Output,
  server: &MockServer,
  config_path: &Path,
  conclusion: &str,
) -> TestResult<()> {
  assert_eq!(out.status.code(), Some(0), "stderr: {}", text(&out.stderr));
  assert_eq!(count(server, "POST", "/acquirejob").await?, 1);
  let mint = format!("{RUNNERS_PATH}/generate-jitconfig");
  assert_eq!(count(server, "POST", &mint).await?, 0, "no re-mint");
  assert_eq!(count(server, "DELETE", &delete_path()).await?, 1);
  assert_eq!(reported_conclusion(server).await?, conclusion);
  assert_state(config_path, false, out);
  assert!(
    text(&out.stdout).contains("unregistered on GitHub"),
    "{}",
    text(&out.stdout)
  );
  Ok(())
}

/// AC-5: a successful job.
#[tokio::test]
async fn successful_job_unregisters_and_removes_the_registration() -> TestResult<()> {
  let home = tempfile::tempdir()?;
  let server = job_server("true", 204).await?;
  let config_path = write_registration(home.path(), &server)?;
  let out = wait_exit(spawn_run(home.path(), &config_path, Some(TOKEN))?).await?;
  assert_ran_once_and_cleaned(&out, &server, &config_path, "succeeded").await
}

/// AC-5: a failing job still ends the ephemeral registration (exit 0, like
/// upstream and `--once`; the failure is reported to GitHub).
#[tokio::test]
async fn failed_job_unregisters_and_removes_the_registration() -> TestResult<()> {
  let home = tempfile::tempdir()?;
  let server = job_server("exit 3", 204).await?;
  let config_path = write_registration(home.path(), &server)?;
  let out = wait_exit(spawn_run(home.path(), &config_path, Some(TOKEN))?).await?;
  assert_ran_once_and_cleaned(&out, &server, &config_path, "failed").await
}

/// AC-5: SIGTERM mid-job stops it; the job consumed the registration, so it
/// is still unregistered and removed. The reported conclusion is `failed`:
/// that is toolu's existing result for an operator shutdown mid-job, which
/// #92 does not change.
#[tokio::test]
async fn cancelled_job_unregisters_and_removes_the_registration() -> TestResult<()> {
  let home = tempfile::tempdir()?;
  let server = job_server("sleep 30", 204).await?;
  let config_path = write_registration(home.path(), &server)?;
  let child = spawn_run(home.path(), &config_path, Some(TOKEN))?;
  wait_for(&server, "POST", "/acquirejob").await?;
  tokio::time::sleep(Duration::from_secs(1)).await;
  sigterm(&child)?;
  let out = wait_exit(child).await?;
  assert_ran_once_and_cleaned(&out, &server, &config_path, "failed").await
}

/// AC-6: SIGTERM before any job keeps the registration (no DELETE); a
/// restarted `run` then takes its one job and cleans up.
#[tokio::test]
async fn restart_before_the_first_job_keeps_then_uses_the_registration() -> TestResult<()> {
  let home = tempfile::tempdir()?;
  let server = idle_server().await?;
  let config_path = write_registration(home.path(), &server)?;
  let child = spawn_run(home.path(), &config_path, Some(TOKEN))?;
  wait_for(&server, "GET", "/message").await?;
  sigterm(&child)?;
  let out = wait_exit(child).await?;
  assert_eq!(out.status.code(), Some(0), "stderr: {}", text(&out.stderr));
  assert_eq!(count(&server, "DELETE", &delete_path()).await?, 0);
  assert_state(&config_path, true, &out);

  server.reset().await;
  boot_fixtures::mount_auth_and_session(&server).await;
  boot_fixtures::mount_job_lifecycle(&server, "true").await?;
  mount_runners_api(&server, 204).await;
  let out = wait_exit(spawn_run(home.path(), &config_path, Some(TOKEN))?).await?;
  assert_ran_once_and_cleaned(&out, &server, &config_path, "succeeded").await
}

/// AC-7: a failing unregister keeps the only handle and names the retry.
#[tokio::test]
async fn failed_unregister_keeps_the_registration() -> TestResult<()> {
  let home = tempfile::tempdir()?;
  let server = job_server("true", 500).await?;
  let config_path = write_registration(home.path(), &server)?;
  let out = wait_exit(spawn_run(home.path(), &config_path, Some(TOKEN))?).await?;
  let stderr = text(&out.stderr);
  assert_eq!(out.status.code(), Some(2), "stderr: {stderr}");
  assert!(stderr.contains("toolu-runner remove"), "{stderr}");
  assert!(!text(&out.stdout).contains("unregistered on GitHub"));
  assert_eq!(count(&server, "DELETE", &delete_path()).await?, 1);
  assert_state(&config_path, true, &out);
  Ok(())
}

/// AC-7: GitHub already removed the used JIT runner (404, confirmed by an
/// empty lookup) — that is a successful unregister.
#[tokio::test]
async fn already_removed_runner_counts_as_unregistered() -> TestResult<()> {
  let home = tempfile::tempdir()?;
  let server = job_server("true", 404).await?;
  let config_path = write_registration(home.path(), &server)?;
  let out = wait_exit(spawn_run(home.path(), &config_path, Some(TOKEN))?).await?;
  assert_ran_once_and_cleaned(&out, &server, &config_path, "succeeded").await?;
  assert_eq!(
    count(&server, "GET", RUNNERS_PATH).await?,
    1,
    "404 confirmed by lookup"
  );
  Ok(())
}

/// AC-7: without a token the state goes, and the output says GitHub-side
/// removal was not verified (no DELETE was possible).
#[tokio::test]
async fn no_token_removes_local_state_and_says_unverified() -> TestResult<()> {
  let home = tempfile::tempdir()?;
  let server = job_server("true", 204).await?;
  let config_path = write_registration(home.path(), &server)?;
  let out = wait_exit(spawn_run(home.path(), &config_path, None)?).await?;
  assert_eq!(out.status.code(), Some(0), "stderr: {}", text(&out.stderr));
  assert_eq!(count(&server, "DELETE", &delete_path()).await?, 0);
  assert!(
    text(&out.stdout).contains("not verified"),
    "{}",
    text(&out.stdout)
  );
  assert_state(&config_path, false, &out);
  Ok(())
}

/// AC-12: the runner was removed on GitHub before any job — the token
/// exchange answers with the live capture (400, OAuth `invalid_client`).
/// `run` exits 1, keeps state, never DELETEs or re-mints, and names
/// `toolu-runner remove` instead of retrying the dead registration forever.
#[tokio::test]
async fn dead_registration_before_a_job_exits_1_and_keeps_state() -> TestResult<()> {
  let home = tempfile::tempdir()?;
  let server = MockServer::start().await;
  Mock::given(method("POST"))
    .and(path("/_apis/distributedtracing/oauth2/token"))
    .respond_with(invalid_client_capture()?)
    .mount(&server)
    .await;
  mount_runners_api(&server, 204).await;
  let config_path = write_registration(home.path(), &server)?;
  let out = wait_exit(spawn_run(home.path(), &config_path, Some(TOKEN))?).await?;
  let stderr = text(&out.stderr);
  assert_eq!(out.status.code(), Some(1), "stderr: {stderr}");
  assert!(stderr.contains("toolu-runner remove"), "{stderr}");
  assert_eq!(count(&server, "DELETE", &delete_path()).await?, 0);
  let mint = format!("{RUNNERS_PATH}/generate-jitconfig");
  assert_eq!(count(&server, "POST", &mint).await?, 0);
  assert_state(&config_path, true, &out);
  Ok(())
}

/// GitHub's live 409 reply to a second session for one registration.
const CONFLICT_CAPTURE: &str = include_str!("../../wire/tests/session_conflict_409.json");

/// A broker whose `POST /session` answers `template` (token exchange mounted).
async fn session_failing_server(template: ResponseTemplate) -> TestResult<MockServer> {
  let server = MockServer::start().await;
  boot_fixtures::mount_auth(&server).await;
  Mock::given(method("POST"))
    .and(path("/session"))
    .respond_with(template)
    .mount(&server)
    .await;
  mount_runners_api(&server, 204).await;
  Ok(server)
}

/// Assert an exit that kept the registration and never unregistered or
/// re-minted.
async fn assert_kept(
  out: &Output,
  server: &MockServer,
  config_path: &Path,
  code: i32,
) -> TestResult<()> {
  assert_eq!(
    out.status.code(),
    Some(code),
    "stderr: {}",
    text(&out.stderr)
  );
  assert_eq!(count(server, "DELETE", &delete_path()).await?, 0);
  let mint = format!("{RUNNERS_PATH}/generate-jitconfig");
  assert_eq!(count(server, "POST", &mint).await?, 0);
  assert_state(config_path, true, out);
  Ok(())
}

/// An org-level registration: the repo-scoped runners API cannot address it,
/// so the local state goes and the output says GitHub-side removal was not
/// verified (no DELETE is sent).
#[tokio::test]
async fn org_registration_removes_local_state_and_says_unverified() -> TestResult<()> {
  let home = tempfile::tempdir()?;
  let server = job_server("true", 204).await?;
  let config_path = write_registration_at(home.path(), &server, "octo")?;
  let out = wait_exit(spawn_run(home.path(), &config_path, Some(TOKEN))?).await?;
  assert_eq!(out.status.code(), Some(0), "stderr: {}", text(&out.stderr));
  assert_eq!(count(&server, "POST", "/acquirejob").await?, 1);
  assert_eq!(count(&server, "DELETE", &delete_path()).await?, 0);
  let stdout = text(&out.stdout);
  assert!(
    stdout.contains("not verified here (no runners API endpoint"),
    "{stdout}"
  );
  assert_state(&config_path, false, &out);
  Ok(())
}

/// A transient failure before any job backs off and retries the same,
/// unused registration (no re-mint); the retry then takes the job.
#[tokio::test]
async fn transient_failure_before_a_job_retries_the_same_registration() -> TestResult<()> {
  let home = tempfile::tempdir()?;
  let server = job_server("true", 204).await?;
  Mock::given(method("POST"))
    .and(path("/session"))
    .respond_with(ResponseTemplate::new(500))
    .up_to_n_times(1)
    .with_priority(1)
    .mount(&server)
    .await;
  let config_path = write_registration(home.path(), &server)?;
  let out = wait_exit(spawn_run(home.path(), &config_path, Some(TOKEN))?).await?;
  assert_eq!(
    count(&server, "POST", "/session").await?,
    2,
    "one failure, one retry"
  );
  assert_ran_once_and_cleaned(&out, &server, &config_path, "succeeded").await
}

/// `--once` keeps its single-lifecycle meaning: a transient failure exits 1
/// at once (no backoff), keeping the unused registration.
#[tokio::test]
async fn once_with_a_transient_failure_exits_1_and_keeps_state() -> TestResult<()> {
  let home = tempfile::tempdir()?;
  let server = session_failing_server(ResponseTemplate::new(500)).await?;
  let config_path = write_registration(home.path(), &server)?;
  let child = spawn_run_with(home.path(), &config_path, Some(TOKEN), &["--once"])?;
  let out = wait_exit(child).await?;
  assert_eq!(
    count(&server, "POST", "/session").await?,
    1,
    "no retry under --once"
  );
  assert_kept(&out, &server, &config_path, 1).await
}

/// A `remove` that ran while this `run` held the lock (`.pending_remove`)
/// ends the loop after the failed lifecycle instead of retrying; nothing was
/// used, so the registration is kept for that `remove`.
#[tokio::test]
async fn pending_remove_exits_0_and_keeps_state() -> TestResult<()> {
  let home = tempfile::tempdir()?;
  let server = session_failing_server(ResponseTemplate::new(500)).await?;
  let config_path = write_registration(home.path(), &server)?;
  let dir = config_path.parent().ok_or("registration dir")?;
  std::fs::write(dir.join(".pending_remove"), "{}")?;
  let out = wait_exit(spawn_run(home.path(), &config_path, Some(TOKEN))?).await?;
  assert_eq!(count(&server, "POST", "/session").await?, 1);
  assert_kept(&out, &server, &config_path, 0).await
}

/// A session conflict that outlasts the bounded retry exits 5 (as `run`
/// and `boot` do), keeping the registration.
#[tokio::test]
async fn session_conflict_exits_5_and_keeps_state() -> TestResult<()> {
  let capture: Value = serde_json::from_str(CONFLICT_CAPTURE)?;
  let status = capture
    .pointer("/response/status")
    .and_then(Value::as_u64)
    .ok_or("status")?;
  let home = tempfile::tempdir()?;
  let server = session_failing_server(ResponseTemplate::new(u16::try_from(status)?)).await?;
  let config_path = write_registration(home.path(), &server)?;
  let out = wait_exit(spawn_run(home.path(), &config_path, Some(TOKEN))?).await?;
  assert_eq!(count(&server, "POST", "/session").await?, 8);
  assert_kept(&out, &server, &config_path, 5).await
}

/// The job was acquired and ran, but reporting its completion failed: the
/// job still consumed the registration, so it is unregistered and removed,
/// and `run` exits 1 naming the error.
#[tokio::test]
async fn acquired_job_with_failed_completion_still_cleans_up() -> TestResult<()> {
  let home = tempfile::tempdir()?;
  let server = job_server("true", 204).await?;
  Mock::given(method("POST"))
    .and(path("/completejob"))
    .respond_with(ResponseTemplate::new(400))
    .with_priority(1)
    .mount(&server)
    .await;
  let config_path = write_registration(home.path(), &server)?;
  let out = wait_exit(spawn_run(home.path(), &config_path, Some(TOKEN))?).await?;
  let stderr = text(&out.stderr);
  assert_eq!(out.status.code(), Some(1), "stderr: {stderr}");
  assert!(stderr.contains("the job was acquired"), "{stderr}");
  assert_eq!(count(&server, "POST", "/acquirejob").await?, 1);
  assert_eq!(count(&server, "DELETE", &delete_path()).await?, 1);
  assert_state(&config_path, false, &out);
  Ok(())
}
