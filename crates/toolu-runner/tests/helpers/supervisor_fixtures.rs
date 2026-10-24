//! Shared fixtures for the `run --all` supervisor tests (#208).
//!
//! Each registration is a real persisted `config.toml` + `credentials.json`
//! under a temporary runner home, whose JIT envelope (a real RSA keypair,
//! `boot_fixtures::real_jit_config_b64`) and runners API both point at its
//! own wiremock peer. The real binary runs against them; nothing in the
//! runner is stubbed. Supervisor stderr goes to a file in the home (a pipe
//! nobody drains would block a long-running child).

use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::time::{Duration, Instant};

use config::config::{
  CacheSection, CredentialsFile, RunnerRegistrationConfig, RuntimeConfig, ServicesSection,
  ShadowSection, WorkspaceSection, save_config, save_credentials,
};
use serde_json::json;
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::boot_fixtures;

/// Boxed error for helpers that use `?`.
pub type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

/// Budget for any one awaited condition.
pub const BUDGET: Duration = Duration::from_secs(60);
/// The bearer the runners API accepts (unregister, re-mint).
pub const TOKEN: &str = "supervisor-208-token";
/// Every fixture registration's persisted runner id.
pub const RUNNER_ID: i64 = 501;

/// Which runner a fixture registration serves.
#[derive(Clone, Copy)]
pub enum Scope<'a> {
  /// `runners/<owner>/<repo>/`, URL `<peer>/<owner>/<repo>`.
  Repo(&'a str, &'a str),
  /// `orgs/<org>/`, URL `<peer>/<org>`.
  Org(&'a str),
}

impl Scope<'_> {
  /// The registration dir under `home`.
  pub fn dir(self, home: &Path) -> PathBuf {
    match self {
      Self::Repo(owner, repo) => home.join("runners").join(owner).join(repo),
      Self::Org(org) => home.join("orgs").join(org),
    }
  }

  /// The URL path after the peer origin.
  fn url_path(self) -> String {
    match self {
      Self::Repo(owner, repo) => format!("{owner}/{repo}"),
      Self::Org(org) => org.to_owned(),
    }
  }
}

/// Persist a registration for `scope` pointing at `server`; returns its
/// `config.toml` path.
pub fn write_registration(
  home: &Path,
  server: &MockServer,
  scope: Scope<'_>,
  ephemeral: bool,
) -> TestResult<PathBuf> {
  let dir = scope.dir(home);
  std::fs::create_dir_all(dir.join("_diag"))?;
  let config_path = dir.join("config.toml");
  let cfg = RunnerRegistrationConfig {
    runner_url: format!("{}/{}", server.uri(), scope.url_path()),
    runner_name: format!("toolu-208-{}", scope.url_path().replace('/', "-")),
    runner_id: RUNNER_ID,
    auth_token: "test-client-id".to_owned(),
    labels: vec!["self-hosted".to_owned()],
    runner_group: "Default".to_owned(),
    ephemeral,
    runtime: RuntimeConfig {
      jit_config: boot_fixtures::real_jit_config_b64(&server.uri())?,
      work_dir: home.join("_work").to_string_lossy().into_owned(),
      data_dir: dir.to_string_lossy().into_owned(),
      protocol_version: "v2".to_owned(),
    },
    services: ServicesSection::default(),
    cache: CacheSection::default(),
    workspace: WorkspaceSection::default(),
    shadow: ShadowSection::default(),
    network: config::network::NetworkSection::default(),
    auth: config::registration_auth::AuthSection::default(),
    provider: config::provider::ProviderSection::Github,
  };
  save_config(&config_path, &cfg)?;
  save_credentials(
    &dir.join("credentials.json"),
    &CredentialsFile {
      access_token: "test-client-id".to_owned(),
      issued_at: "2026-10-23T00:00:00Z".to_owned(),
      expires_at: None,
    },
  )?;
  Ok(config_path)
}

/// Mount the idle long-poll (202 after 200 ms), the session delete, and the
/// runners API: DELETE of the runner 204, an empty name lookup, and a
/// `generate-jitconfig` that mints a fresh real JIT for this same peer.
async fn mount_idle_and_runners_api(server: &MockServer) -> TestResult<()> {
  Mock::given(method("GET"))
    .and(path("/message"))
    .respond_with(ResponseTemplate::new(202).set_delay(Duration::from_millis(200)))
    .mount(server)
    .await;
  Mock::given(method("DELETE"))
    .and(path_regex("^/session/"))
    .respond_with(ResponseTemplate::new(200))
    .mount(server)
    .await;
  Mock::given(method("DELETE"))
    .and(path_regex(r"^/api/v3/.+/actions/runners/\d+$"))
    .respond_with(ResponseTemplate::new(204))
    .mount(server)
    .await;
  Mock::given(method("GET"))
    .and(path_regex("^/api/v3/.+/actions/runners$"))
    .respond_with(
      ResponseTemplate::new(200).set_body_json(json!({"total_count": 0, "runners": []})),
    )
    .mount(server)
    .await;
  Mock::given(method("POST"))
    .and(path_regex(
      "^/api/v3/.+/actions/runners/generate-jitconfig$",
    ))
    .respond_with(ResponseTemplate::new(201).set_body_json(json!({
      "runner": {"id": RUNNER_ID + 1, "name": "re-minted"},
      "encoded_jit_config": boot_fixtures::real_jit_config_b64(&server.uri())?,
    })))
    .mount(server)
    .await;
  Ok(())
}

/// A peer with a live session and no job.
pub async fn idle_broker() -> TestResult<MockServer> {
  let server = MockServer::start().await;
  boot_fixtures::mount_auth_and_session(&server).await;
  mount_idle_and_runners_api(&server).await?;
  Ok(server)
}

/// A peer serving one real job running `script`, then idle.
pub async fn job_broker(script: &str) -> TestResult<MockServer> {
  let server = MockServer::start().await;
  boot_fixtures::mount_auth_and_session(&server).await;
  boot_fixtures::mount_job_once(&server, script).await?;
  mount_idle_and_runners_api(&server).await?;
  Ok(server)
}

/// GitHub's captured 401 reply to a rejected bearer
/// (`fixtures/doctor/github_user_401.json`).
const BAD_CREDENTIALS: &str = include_str!("../fixtures/doctor/github_user_401.json");

/// A peer serving one real job running `script`, whose re-mint
/// (`generate-jitconfig`) GitHub rejects with the captured 401.
pub async fn job_broker_rejecting_remint(script: &str) -> TestResult<MockServer> {
  let server = MockServer::start().await;
  Mock::given(method("POST"))
    .and(path_regex(
      "^/api/v3/.+/actions/runners/generate-jitconfig$",
    ))
    .respond_with(
      ResponseTemplate::new(401)
        .insert_header("content-type", "application/json; charset=utf-8")
        .set_body_string(BAD_CREDENTIALS),
    )
    .with_priority(1)
    .mount(&server)
    .await;
  boot_fixtures::mount_auth_and_session(&server).await;
  boot_fixtures::mount_job_once(&server, script).await?;
  mount_idle_and_runners_api(&server).await?;
  Ok(server)
}

/// A peer whose `POST /session` always answers the live-captured 409.
pub async fn conflict_broker() -> TestResult<MockServer> {
  let server = MockServer::start().await;
  boot_fixtures::mount_auth(&server).await;
  Mock::given(method("POST"))
    .and(path("/session"))
    .respond_with(crate::session_conflict_e2e_test::captured_conflict()?)
    .mount(&server)
    .await;
  Ok(server)
}

/// The runner binary pinned to `home` (both `HOME` and `TOOLU_RUNNER_HOME`),
/// with `TOOLU_RUNNER_TOKEN` set to [`TOKEN`] when `token`.
pub fn runner(home: &Path, token: bool) -> Command {
  let mut cmd = Command::new(env!("CARGO_BIN_EXE_toolu-runner"));
  cmd
    .env("HOME", home)
    .env("TOOLU_RUNNER_HOME", home)
    .env("TOOLU_RUNNER_NO_KEYRING", "1")
    .env("TOOLU_RUNNER_SESSION_CONFLICT_INTERVAL_MS", "50")
    .env_remove("TOOLU_RUNNER_TOKEN")
    .stdin(Stdio::null());
  if token {
    cmd.env("TOOLU_RUNNER_TOKEN", TOKEN);
  }
  cmd
}

/// A spawned runner, killed (and reaped) if the test ends without
/// stopping it — a failed assertion must not orphan a supervisor.
pub struct Spawned(Option<Child>);

impl Spawned {
  /// The child's pid.
  pub fn id(&self) -> u32 {
    self.0.as_ref().map_or(0, Child::id)
  }

  /// `Child::try_wait` — `Some` once the child exited.
  pub fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
    match self.0.as_mut() {
      Some(child) => child.try_wait(),
      None => Ok(None),
    }
  }

  /// Hand the child over, disarming the kill.
  fn take(mut self) -> TestResult<Child> {
    self.0.take().ok_or_else(|| "child already taken".into())
  }
}

impl Drop for Spawned {
  fn drop(&mut self) {
    if let Some(mut child) = self.0.take() {
      // Best effort: the test is already failing or done with it.
      if child.kill().is_ok() {
        drop(child.wait());
      }
    }
  }
}

/// Spawn `run <args>` with stdout/stderr captured to `<home>/<name>.log`.
pub fn spawn_logged(home: &Path, token: bool, name: &str, args: &[&str]) -> TestResult<Spawned> {
  spawn_command(runner(home, token), home, name, args)
}

/// [`spawn_logged`] without a token and with the extra environment `envs`.
pub fn spawn_logged_with(
  home: &Path,
  name: &str,
  args: &[&str],
  envs: &[(&str, &str)],
) -> TestResult<Spawned> {
  let mut cmd = runner(home, false);
  cmd.envs(envs.iter().copied());
  spawn_command(cmd, home, name, args)
}

/// Spawn `cmd <args>` with stdout/stderr captured to `<home>/<name>.log`.
fn spawn_command(mut cmd: Command, home: &Path, name: &str, args: &[&str]) -> TestResult<Spawned> {
  let log = std::fs::File::create(home.join(format!("{name}.log")))?;
  cmd.args(args).stdout(log.try_clone()?).stderr(log);
  Ok(Spawned(Some(cmd.spawn()?)))
}

/// Spawn `run --all` (log: `<home>/supervisor.log`).
pub fn spawn_run_all(home: &Path, token: bool) -> TestResult<Spawned> {
  spawn_logged(home, token, "supervisor", &["run", "--all"])
}

/// What a spawned child wrote to `<home>/<name>.log`.
pub fn log_of(home: &Path, name: &str) -> String {
  let path = home.join(format!("{name}.log"));
  std::fs::read_to_string(&path)
    .unwrap_or_else(|e| format!("<cannot read {}: {e}>", path.display()))
}

/// Requests `server` received matching `method_name` and a path starting
/// with `prefix`.
pub async fn count(server: &MockServer, method_name: &str, prefix: &str) -> TestResult<usize> {
  let requests = server
    .received_requests()
    .await
    .ok_or("recording disabled")?;
  Ok(
    requests
      .iter()
      .filter(|r| r.method.as_str() == method_name && r.url.path().starts_with(prefix))
      .count(),
  )
}

/// Wait until `server` has seen at least `min` matching requests.
pub async fn wait_for(
  server: &MockServer,
  method_name: &str,
  prefix: &str,
  min: usize,
) -> TestResult<()> {
  let deadline = Instant::now() + BUDGET;
  while count(server, method_name, prefix).await? < min {
    if Instant::now() > deadline {
      return Err(format!("never saw {min} x {method_name} {prefix}").into());
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
  }
  Ok(())
}

/// Wait until `condition` holds, or fail naming `what`.
pub async fn wait_until(what: &str, mut condition: impl FnMut() -> bool) -> TestResult<()> {
  let deadline = Instant::now() + BUDGET;
  while !condition() {
    if Instant::now() > deadline {
      return Err(format!("timed out waiting for {what}").into());
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
  }
  Ok(())
}

/// The pid in the registration's `.lock` body, if any.
pub fn lock_pid(config_path: &Path) -> Option<u32> {
  let dir = config_path.parent()?;
  let raw = std::fs::read_to_string(dir.join(".lock")).ok()?;
  let body: serde_json::Value = serde_json::from_str(&raw).ok()?;
  body
    .get("pid")
    .and_then(serde_json::Value::as_u64)
    .and_then(|pid| u32::try_from(pid).ok())
}

/// Whether another process holds the registration's `.lock` exclusively.
pub fn lock_is_held(config_path: &Path) -> TestResult<bool> {
  let dir = config_path.parent().ok_or("config has no parent")?;
  let file = std::fs::File::open(dir.join(".lock"))?;
  Ok(file.try_lock_shared().is_err())
}

/// Send `signal` (e.g. `-TERM`, `-HUP`) to `child`.
pub fn signal(child: &Spawned, signal: &str) -> TestResult<()> {
  let status = Command::new("kill")
    .args([signal, &child.id().to_string()])
    .status()?;
  if status.success() {
    Ok(())
  } else {
    Err(format!("kill {signal} failed").into())
  }
}

/// Wait for `child` to exit within [`BUDGET`], killing it otherwise.
pub async fn wait_exit(child: Spawned) -> TestResult<Output> {
  let mut child = child.take()?;
  let deadline = Instant::now() + BUDGET;
  while child.try_wait()?.is_none() {
    if Instant::now() > deadline {
      child.kill()?;
      return Err("process did not exit within the budget".into());
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
  }
  Ok(child.wait_with_output()?)
}

/// SIGTERM `child` and require a clean exit 0 (its log in the error).
pub async fn stop(child: Spawned, home: &Path, name: &str) -> TestResult<()> {
  signal(&child, "-TERM")?;
  let out = wait_exit(child).await?;
  if out.status.code() == Some(0) {
    Ok(())
  } else {
    Err(format!("exit {:?}; log:\n{}", out.status.code(), log_of(home, name)).into())
  }
}

/// The `conclusion` the runner reported in its `completejob`.
pub async fn reported_conclusion(server: &MockServer) -> TestResult<String> {
  let requests = server
    .received_requests()
    .await
    .ok_or("recording disabled")?;
  let complete = requests
    .iter()
    .find(|r| r.url.path() == "/completejob")
    .ok_or("no completejob request")?;
  let body: serde_json::Value = serde_json::from_slice(&complete.body)?;
  let conclusion = body.get("conclusion").and_then(serde_json::Value::as_str);
  Ok(conclusion.ok_or("no conclusion")?.to_owned())
}
