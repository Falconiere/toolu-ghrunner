//! #201 the always-online `run` loop re-minting with a GitHub App (S3, S4;
//! AC-6 to AC-10) through the real binary.
//!
//! One loopback peer is the whole of GitHub: the broker (`boot_fixtures`,
//! a real JIT keypair, one real shell step per job), the App endpoints
//! replaying `fixtures/github_app/`, and a `generate-jitconfig` that answers
//! only the installation token with a fresh real envelope pointing back at
//! the peer, so the loop keeps taking jobs. No stored login and no
//! `TOOLU_RUNNER_TOKEN` exist for the App cases.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use config::config::{
  CacheSection, CredentialsFile, RunnerRegistrationConfig, RuntimeConfig, ServicesSection,
  ShadowSection, WorkspaceSection, save_config, save_credentials,
};
use config::registration_auth::AuthSection;
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use crate::boot_fixtures;
use crate::github_app_fixtures::{
  INSTALLATION_TOKEN, PRIVATE_PEM, authorizations, capture, rfc3339_in, token_reply, write_app,
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const TOKENS: &str = "/api/v3/app/installations/1/access_tokens";
const JIT: &str = "/api/v3/repos/octo/demo/actions/runners/generate-jitconfig";
const INSTALL_URL: &str = "https://github.com/apps/toolu-runner-test-app/installations/new";
const EXIT_BUDGET: Duration = Duration::from_secs(120);
/// The runner id every re-mint returns.
const REMINTED_ID: i64 = 777;

/// A broker serving a one-step job on every poll, plus a
/// `generate-jitconfig` answering only `authorization`.
async fn loop_server(authorization: &str) -> TestResult<MockServer> {
  loop_server_running(authorization, "true").await
}

/// [`loop_server`] whose job step runs `script`.
async fn loop_server_running(authorization: &str, script: &str) -> TestResult<MockServer> {
  let server = MockServer::start().await;
  boot_fixtures::mount_auth_and_session(&server).await;
  boot_fixtures::mount_job_lifecycle(&server, script).await?;
  let reminted = json!({
    "runner": {"id": REMINTED_ID, "name": "app-remint-201"},
    "encoded_jit_config": boot_fixtures::real_jit_config_b64(&server.uri())?,
  });
  Mock::given(method("POST"))
    .and(path(JIT))
    .and(header("authorization", authorization))
    .respond_with(ResponseTemplate::new(201).set_body_json(reminted))
    .mount(&server)
    .await;
  Ok(server)
}

/// Persist a persistent registration for `<server>/octo/demo` with `auth`
/// and NON-default `[cache]` / `[workspace]` / `[services] bind` / labels,
/// so a re-mint that clobbered any of them is caught.
fn write_registration(home: &Path, server: &MockServer, auth: AuthSection) -> TestResult<PathBuf> {
  let dir = home.join("runners/octo/demo");
  std::fs::create_dir_all(&dir)?;
  let config_path = dir.join("config.toml");
  let cfg = RunnerRegistrationConfig {
    runner_url: format!("{}/octo/demo", server.uri()),
    runner_name: "app-remint-201".to_owned(),
    runner_id: 501,
    auth_token: "test-client-id".to_owned(),
    labels: vec!["self-hosted".to_owned(), "app-201".to_owned()],
    runner_group: "Default".to_owned(),
    ephemeral: false,
    runtime: RuntimeConfig {
      jit_config: boot_fixtures::real_jit_config_b64(&server.uri())?,
      work_dir: dir.join("_work").to_string_lossy().into_owned(),
      data_dir: dir.to_string_lossy().into_owned(),
      protocol_version: "v2".to_owned(),
    },
    services: ServicesSection {
      bind: "0.0.0.0:9201".to_owned(),
      ..ServicesSection::default()
    },
    cache: CacheSection {
      max_bytes: 123_456_789,
      ..CacheSection::default()
    },
    workspace: WorkspaceSection {
      gc_after_hours: 72,
      ..WorkspaceSection::default()
    },
    shadow: ShadowSection::default(),
    network: config::network::NetworkSection::default(),
    auth,
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

/// An App registration (installation 1) whose App file lives in `home`.
fn app_auth(home: &Path) -> TestResult<AuthSection> {
  let app_file = write_app(home, "127.0.0.1")?;
  Ok(AuthSection::App {
    app_file: app_file.to_string_lossy().into_owned(),
    installation_id: 1,
  })
}

/// Spawn `run --config <path>` (no `--once`) with no login and, unless
/// given, no `TOOLU_RUNNER_TOKEN`.
fn spawn_run(home: &Path, config_path: &Path, env_token: Option<&str>) -> TestResult<Child> {
  spawn_run_with(home, config_path, env_token, &[])
}

/// [`spawn_run`] with extra `run` arguments (e.g. `--once`).
fn spawn_run_with(
  home: &Path,
  config_path: &Path,
  env_token: Option<&str>,
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
    .args(["run", "--config"])
    .arg(config_path)
    .args(extra);
  if let Some(token) = env_token {
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

/// Requests `server` saw at `path_name` (any method).
async fn hits(server: &MockServer, path_name: &str) -> TestResult<usize> {
  let requests = server.received_requests().await.ok_or("recording")?;
  Ok(
    requests
      .iter()
      .filter(|r| r.url.path() == path_name)
      .count(),
  )
}

/// Wait until `server` saw `path_name` at least `n` times.
async fn wait_for(server: &MockServer, path_name: &str, n: usize) -> TestResult {
  let deadline = Instant::now() + EXIT_BUDGET;
  while hits(server, path_name).await? < n {
    if Instant::now() > deadline {
      return Err(format!("never saw {n} × {path_name}").into());
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
  }
  Ok(())
}

/// SIGTERM `child` (the loop exits 0 on cancel).
fn sigterm(child: &Child) -> TestResult {
  let status = Command::new("kill")
    .args(["-TERM", &child.id().to_string()])
    .status()?;
  if status.success() {
    Ok(())
  } else {
    Err("kill -TERM failed".into())
  }
}

fn text(bytes: &[u8]) -> String {
  String::from_utf8_lossy(bytes).into_owned()
}

/// `config.toml` minus the three fields a re-mint may change.
fn without_mint_fields(config_path: &Path) -> TestResult<String> {
  Ok(
    std::fs::read_to_string(config_path)?
      .lines()
      .filter(|line| {
        !["runner_id = ", "auth_token = ", "jit_config = "]
          .iter()
          .any(|field| line.starts_with(field))
      })
      .collect::<Vec<_>>()
      .join("\n"),
  )
}

/// Every `runner.log*` under the runner's `HOME` (`~/.toolu-runner/_diag/`).
fn runner_log(home: &Path) -> TestResult<String> {
  let mut combined = String::new();
  for entry in std::fs::read_dir(home.join(".toolu-runner/_diag"))? {
    let entry = entry?;
    if entry
      .file_name()
      .to_str()
      .is_some_and(|name| name.starts_with("runner.log"))
    {
      combined.push_str(&std::fs::read_to_string(entry.path())?);
    }
  }
  Ok(combined)
}

/// AC-6 (S3): two jobs, two re-mints through ONE installation token (cached),
/// every `generate-jitconfig` authenticated with it, every section but the
/// three mint fields byte-identical, and no login warning.
#[tokio::test]
async fn an_app_registration_re_mints_after_each_job_with_a_cached_token() -> TestResult {
  let home = tempfile::tempdir()?;
  let server = loop_server(&format!("Bearer {INSTALLATION_TOKEN}")).await?;
  Mock::given(method("POST"))
    .and(path(TOKENS))
    .respond_with(token_reply(&rfc3339_in(3600))?)
    .mount(&server)
    .await;
  let config_path = write_registration(home.path(), &server, app_auth(home.path())?)?;
  let before = without_mint_fields(&config_path)?;

  let child = spawn_run(home.path(), &config_path, None)?;
  wait_for(&server, JIT, 2).await?;
  sigterm(&child)?;
  let out = wait_exit(child).await?;
  assert_eq!(out.status.code(), Some(0), "stderr: {}", text(&out.stderr));

  assert!(hits(&server, "/completejob").await? >= 2);
  assert_eq!(hits(&server, TOKENS).await?, 1, "the token is reused");
  let bearers = authorizations(&server, JIT).await?;
  assert!(bearers.len() >= 2);
  assert!(
    bearers
      .iter()
      .all(|b| *b == format!("Bearer {INSTALLATION_TOKEN}"))
  );
  assert_eq!(without_mint_fields(&config_path)?, before);
  let cfg = config::config::load_config(&config_path)?;
  assert_eq!(cfg.runner_id, REMINTED_ID);
  assert!(!cfg.auth.is_token(), "[auth] survives the re-mint");
  assert!(
    !text(&out.stderr).contains("no stored GitHub login"),
    "an App registration needs no login: {}",
    text(&out.stderr)
  );
  Ok(())
}

/// Run an App registration whose `access_tokens` answers `reply`, until it
/// exits on its own.
async fn run_until_fatal(reply: ResponseTemplate) -> TestResult<(Output, MockServer)> {
  let home = tempfile::tempdir()?;
  let server = loop_server(&format!("Bearer {INSTALLATION_TOKEN}")).await?;
  Mock::given(method("POST"))
    .and(path(TOKENS))
    .respond_with(reply)
    .mount(&server)
    .await;
  let config_path = write_registration(home.path(), &server, app_auth(home.path())?)?;
  let out = wait_exit(spawn_run(home.path(), &config_path, None)?).await?;
  Ok((out, server))
}

/// AC-7 (S4): GitHub rejecting the App JWT (live 401 capture) ends the loop,
/// naming the fix.
#[tokio::test]
async fn a_rejected_app_ends_the_loop_with_the_fix() -> TestResult {
  let (out, server) = run_until_fatal(capture("access_tokens_401_bad_jwt.json")?).await?;
  assert_eq!(out.status.code(), Some(2), "stdout: {}", text(&out.stdout));
  let stderr = text(&out.stderr);
  for expected in [
    "re-mint rejected",
    "A JSON web token could not be decoded",
    "toolu-runner register --app --replace",
    INSTALL_URL,
  ] {
    assert!(stderr.contains(expected), "missing {expected:?}: {stderr}");
  }
  assert_eq!(hits(&server, "/completejob").await?, 1);
  assert_eq!(hits(&server, JIT).await?, 0);
  Ok(())
}

/// AC-8: a deleted App or removed installation (live 404 capture) is fatal.
#[tokio::test]
async fn an_uninstalled_app_ends_the_loop_with_the_fix() -> TestResult {
  let (out, _server) = run_until_fatal(capture("app_404_integration_not_found.json")?).await?;
  assert_eq!(out.status.code(), Some(2));
  let stderr = text(&out.stderr);
  assert!(stderr.contains("Integration not found"), "{stderr}");
  assert!(
    stderr.contains("toolu-runner register --app --replace"),
    "{stderr}"
  );
  Ok(())
}

/// AC-7 (S4) + AC-10: a 503 backs off and the next re-mint succeeds; the
/// 503 body (here echoing the App's private key) reaches `_diag` masked.
#[tokio::test]
async fn a_503_backs_off_and_the_pem_never_reaches_diag() -> TestResult {
  let home = tempfile::tempdir()?;
  let server = loop_server(&format!("Bearer {INSTALLATION_TOKEN}")).await?;
  Mock::given(method("POST"))
    .and(path(TOKENS))
    .respond_with(ResponseTemplate::new(503).set_body_string(PRIVATE_PEM))
    .up_to_n_times(1)
    .with_priority(1)
    .mount(&server)
    .await;
  Mock::given(method("POST"))
    .and(path(TOKENS))
    .respond_with(token_reply(&rfc3339_in(3600))?)
    .mount(&server)
    .await;
  let config_path = write_registration(home.path(), &server, app_auth(home.path())?)?;

  let child = spawn_run(home.path(), &config_path, None)?;
  wait_for(&server, JIT, 1).await?;
  sigterm(&child)?;
  let out = wait_exit(child).await?;
  assert_eq!(out.status.code(), Some(0), "stderr: {}", text(&out.stderr));
  assert_eq!(hits(&server, TOKENS).await?, 2, "503, then a retried mint");

  let log = runner_log(home.path())?;
  assert!(
    log.contains("re-mint failed; backing off before retry"),
    "the backoff WARN must reach _diag (else the check is vacuous)"
  );
  assert!(log.contains("***"), "the echoed key must appear masked");
  for line in PRIVATE_PEM.lines().filter(|line| line.len() >= 16) {
    assert!(!log.contains(line), "a PEM line reached _diag: {line}");
  }
  Ok(())
}

/// AC-9 control: a token registration (no `[auth]`) still re-mints with
/// `TOOLU_RUNNER_TOKEN` and makes no App call.
#[tokio::test]
async fn a_token_registration_still_re_mints_with_its_token() -> TestResult {
  let home = tempfile::tempdir()?;
  let server = loop_server("Bearer remint-pat").await?;
  let config_path = write_registration(home.path(), &server, AuthSection::Token)?;
  assert!(!std::fs::read_to_string(&config_path)?.contains("[auth]"));

  let child = spawn_run(home.path(), &config_path, Some("remint-pat"))?;
  wait_for(&server, JIT, 1).await?;
  sigterm(&child)?;
  let out = wait_exit(child).await?;
  assert_eq!(out.status.code(), Some(0), "stderr: {}", text(&out.stderr));
  assert_eq!(hits(&server, TOKENS).await?, 0);
  assert!(!std::fs::read_to_string(&config_path)?.contains("[auth]"));
  Ok(())
}

/// An App registration whose App file is gone fails before polling.
#[tokio::test]
async fn a_missing_app_file_fails_before_polling() -> TestResult {
  let home = tempfile::tempdir()?;
  let server = loop_server("Bearer unused").await?;
  let auth = app_auth(home.path())?;
  let config_path = write_registration(home.path(), &server, auth)?;
  std::fs::remove_file(home.path().join("github-app.json"))?;

  let out = wait_exit(spawn_run(home.path(), &config_path, None)?).await?;
  assert_eq!(out.status.code(), Some(2));
  let stderr = text(&out.stderr);
  assert!(stderr.contains("no GitHub App saved at"), "{stderr}");
  assert!(
    stderr.contains("toolu-runner register --app --replace"),
    "{stderr}"
  );
  assert_eq!(hits(&server, "/session").await?, 0, "nothing was polled");
  Ok(())
}

/// A token minted with under ~5 minutes of life left is not reused: every
/// re-mint mints again.
#[tokio::test]
async fn a_token_near_expiry_is_minted_again_for_each_re_mint() -> TestResult {
  let home = tempfile::tempdir()?;
  let server = loop_server(&format!("Bearer {INSTALLATION_TOKEN}")).await?;
  Mock::given(method("POST"))
    .and(path(TOKENS))
    .respond_with(token_reply(&rfc3339_in(200))?)
    .mount(&server)
    .await;
  let config_path = write_registration(home.path(), &server, app_auth(home.path())?)?;
  let child = spawn_run(home.path(), &config_path, None)?;
  wait_for(&server, JIT, 2).await?;
  sigterm(&child)?;
  let out = wait_exit(child).await?;
  assert_eq!(out.status.code(), Some(0), "stderr: {}", text(&out.stderr));
  assert_eq!(
    hits(&server, TOKENS).await?,
    hits(&server, JIT).await?,
    "one mint per re-mint"
  );
  Ok(())
}

/// A 503 whose body echoes the request's `Authorization` header, as a
/// misbehaving proxy might.
struct EchoAuthorization;

impl Respond for EchoAuthorization {
  fn respond(&self, request: &Request) -> ResponseTemplate {
    let authorization = request
      .headers
      .get("authorization")
      .and_then(|value| value.to_str().ok())
      .unwrap_or_default();
    ResponseTemplate::new(503).set_body_string(format!("upstream echoed: {authorization}"))
  }
}

/// AC-10: the App JWT and the installation token reach `_diag` masked when an
/// error body echoes them (the JWT on `access_tokens`, the token on
/// `generate-jitconfig`), and each failure backs off.
#[tokio::test]
async fn app_jwts_and_installation_tokens_never_reach_diag() -> TestResult {
  let home = tempfile::tempdir()?;
  let server = loop_server(&format!("Bearer {INSTALLATION_TOKEN}")).await?;
  Mock::given(method("POST"))
    .and(path(TOKENS))
    .respond_with(EchoAuthorization)
    .up_to_n_times(1)
    .with_priority(1)
    .mount(&server)
    .await;
  Mock::given(method("POST"))
    .and(path(TOKENS))
    .respond_with(token_reply(&rfc3339_in(3600))?)
    .mount(&server)
    .await;
  Mock::given(method("POST"))
    .and(path(JIT))
    .respond_with(EchoAuthorization)
    .up_to_n_times(1)
    .with_priority(1)
    .mount(&server)
    .await;
  let config_path = write_registration(home.path(), &server, app_auth(home.path())?)?;

  let child = spawn_run(home.path(), &config_path, None)?;
  // The echoed 503, then a successful re-mint.
  wait_for(&server, JIT, 2).await?;
  sigterm(&child)?;
  let out = wait_exit(child).await?;
  assert_eq!(out.status.code(), Some(0), "stderr: {}", text(&out.stderr));

  let jwts = authorizations(&server, TOKENS).await?;
  let first_jwt = jwts
    .first()
    .and_then(|header| header.strip_prefix("Bearer "))
    .ok_or("no App JWT was sent")?;
  let log = runner_log(home.path())?;
  assert!(
    log
      .matches("re-mint failed; backing off before retry")
      .count()
      >= 2,
    "both echoed failures must reach _diag (else the check is vacuous)"
  );
  assert!(!log.contains(first_jwt), "an App JWT reached _diag");
  assert!(
    !log.contains(INSTALLATION_TOKEN),
    "an installation token reached _diag"
  );
  assert!(log.contains("upstream echoed: Bearer ***"), "{log}");
  Ok(())
}

/// Write an App file whose `pem` is not a usable key.
fn break_app_key(home: &Path) -> TestResult {
  let path = home.join("github-app.json");
  let mut app: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
  let object = app.as_object_mut().ok_or("App file is not an object")?;
  object.insert(
    "pem".to_owned(),
    serde_json::Value::from("truncated: not a PEM key"),
  );
  std::fs::write(&path, serde_json::to_vec(&app)?)?;
  Ok(())
}

/// An App file whose key cannot sign fails `run` before polling.
#[tokio::test]
async fn an_unusable_app_key_fails_before_polling() -> TestResult {
  let home = tempfile::tempdir()?;
  let server = loop_server("Bearer unused").await?;
  let config_path = write_registration(home.path(), &server, app_auth(home.path())?)?;
  break_app_key(home.path())?;
  let out = wait_exit(spawn_run(home.path(), &config_path, None)?).await?;
  assert_eq!(out.status.code(), Some(2));
  let stderr = text(&out.stderr);
  assert!(stderr.contains("not a usable RSA PEM"), "{stderr}");
  assert_eq!(hits(&server, "/session").await?, 0, "nothing was polled");
  Ok(())
}

/// A key broken while a job runs ends the loop at the re-mint instead of
/// backing off forever on the spent JIT config.
#[tokio::test]
async fn a_key_broken_mid_run_is_fatal_at_the_re_mint() -> TestResult {
  let home = tempfile::tempdir()?;
  let server = loop_server_running(&format!("Bearer {INSTALLATION_TOKEN}"), "sleep 3").await?;
  let config_path = write_registration(home.path(), &server, app_auth(home.path())?)?;
  let child = spawn_run(home.path(), &config_path, None)?;
  wait_for(&server, "/acquirejob", 1).await?;
  break_app_key(home.path())?;
  let out = wait_exit(child).await?;
  assert_eq!(out.status.code(), Some(2), "stdout: {}", text(&out.stdout));
  let stderr = text(&out.stderr);
  assert!(stderr.contains("re-mint rejected"), "{stderr}");
  assert!(stderr.contains("not a usable RSA PEM"), "{stderr}");
  assert_eq!(hits(&server, TOKENS).await?, 0);
  Ok(())
}

/// `--once` never re-mints, so it runs its job without the App file.
#[tokio::test]
async fn once_runs_its_job_without_the_app_file() -> TestResult {
  let home = tempfile::tempdir()?;
  let server = loop_server("Bearer unused").await?;
  let config_path = write_registration(home.path(), &server, app_auth(home.path())?)?;
  std::fs::remove_file(home.path().join("github-app.json"))?;
  let out = wait_exit(spawn_run_with(
    home.path(),
    &config_path,
    None,
    &["--once"],
  )?)
  .await?;
  assert_eq!(out.status.code(), Some(0), "stderr: {}", text(&out.stderr));
  assert_eq!(hits(&server, "/completejob").await?, 1);
  assert_eq!(hits(&server, JIT).await?, 0);
  Ok(())
}
