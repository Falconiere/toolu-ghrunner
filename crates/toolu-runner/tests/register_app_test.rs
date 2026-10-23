//! #201 `register` with a GitHub App through the real binary (S2, AC-2 to
//! AC-5, AC-12).
//!
//! A temp `TOOLU_RUNNER_HOME` holds a real `github-app.json` (the protocol
//! crate's test key). The `--url` host is a loopback peer at the GHES
//! `api/v3` path replaying GitHub's replies (`fixtures/github_app/`) and the
//! committed `generate-jitconfig` envelope. Every App call must carry a JWT
//! that verifies with the key's public half, and `generate-jitconfig` only
//! answers the installation token, so a run that minted with anything else
//! fails.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use config::config::load_config;
use config::registration_auth::AuthSection;
use serde_json::{Value, json};
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::doctor_env::store_login;
use crate::github_app_fixtures::{
  APP_SLUG, INSTALLATION_TOKEN, authorizations, capture, rfc3339_in, token_reply, verify_app_jwt,
  write_app,
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const JIT_FIXTURE: &str = include_str!("fixtures/generate_jitconfig_response.json");
const REPO_INSTALLATION: &str = "/api/v3/repos/octo/demo/installation";
const ORG_INSTALLATION: &str = "/api/v3/orgs/octo-org/installation";
const TOKENS: &str = "/api/v3/app/installations/1/access_tokens";
const REPO_JIT: &str = "/api/v3/repos/octo/demo/actions/runners/generate-jitconfig";
const ORG_JIT: &str = "/api/v3/orgs/octo-org/actions/runners/generate-jitconfig";
const INSTALL_URL: &str = "https://github.com/apps/toolu-runner-test-app/installations/new";

/// The App peer: installation lookups, the token mint, and both JIT
/// endpoints, which answer only the installation token.
async fn app_server() -> TestResult<MockServer> {
  let server = MockServer::start().await;
  for lookup in [REPO_INSTALLATION, ORG_INSTALLATION] {
    Mock::given(method("GET"))
      .and(path(lookup))
      .respond_with(capture("installation_200.json")?)
      .mount(&server)
      .await;
  }
  Mock::given(method("POST"))
    .and(path(TOKENS))
    .respond_with(token_reply(&rfc3339_in(3600))?)
    .mount(&server)
    .await;
  mount_jit(&server, &format!("Bearer {INSTALLATION_TOKEN}")).await?;
  Ok(server)
}

/// Both `generate-jitconfig` endpoints, answering only `authorization`.
async fn mount_jit(server: &MockServer, authorization: &str) -> TestResult {
  let body: Value = serde_json::from_str(JIT_FIXTURE)?;
  for jit in [REPO_JIT, ORG_JIT] {
    Mock::given(method("POST"))
      .and(path(jit))
      .and(header("authorization", authorization))
      .respond_with(ResponseTemplate::new(201).set_body_json(&body))
      .mount(server)
      .await;
  }
  Ok(())
}

/// Run `register --url <server>/<target> <extra…>` in `home` with no
/// personal token in the environment unless `env_token` is given.
fn register(
  home: &Path,
  server: &MockServer,
  target: &str,
  env_token: Option<&str>,
  extra: &[&str],
) -> TestResult<Output> {
  let mut cmd = Command::new(env!("CARGO_BIN_EXE_toolu-runner"));
  cmd
    .env("TOOLU_RUNNER_HOME", home)
    .env("HOME", home)
    .env("TOOLU_RUNNER_NO_KEYRING", "1")
    .env_remove("TOOLU_RUNNER_TOKEN")
    .stdin(Stdio::null())
    .args(["register", "--url", &format!("{}/{target}", server.uri())])
    .args(["--name", "app-201"])
    .args(extra);
  if let Some(token) = env_token {
    cmd.env("TOOLU_RUNNER_TOKEN", token);
  }
  Ok(cmd.output()?)
}

fn repo_config(home: &Path) -> PathBuf {
  home.join("runners/octo/demo/config.toml")
}

fn text(bytes: &[u8]) -> String {
  String::from_utf8_lossy(bytes).into_owned()
}

/// How many requests `server` saw at `path_name`.
async fn hits(server: &MockServer, path_name: &str) -> TestResult<usize> {
  Ok(authorizations(server, path_name).await?.len())
}

/// Assert `home` holds an App registration for installation 1.
fn assert_app_registration(home: &Path, config: &Path) -> TestResult {
  let cfg = load_config(config)?;
  let app_file = std::path::absolute(home.join("github-app.json"))?;
  assert_eq!(
    cfg.auth,
    AuthSection::App {
      app_file: app_file.to_string_lossy().into_owned(),
      installation_id: 1,
    }
  );
  Ok(())
}

/// AC-2 (S2): `--app` resolves the installation, mints a token with a
/// verifiable JWT, mints the JIT with that token (not `TOOLU_RUNNER_TOKEN`),
/// and persists `[auth]`.
#[tokio::test]
async fn register_app_mints_the_jit_with_an_installation_token() -> TestResult {
  let home = tempfile::tempdir()?;
  write_app(home.path(), "127.0.0.1")?;
  let server = app_server().await?;
  let out = register(
    home.path(),
    &server,
    "octo/demo",
    Some("env-pat"),
    &["--app"],
  )?;
  assert!(out.status.success(), "stderr: {}", text(&out.stderr));
  assert!(
    text(&out.stdout).contains(&format!("GitHub App '{APP_SLUG}' (installation 1)")),
    "{}",
    text(&out.stdout)
  );
  for app_call in [REPO_INSTALLATION, TOKENS] {
    let seen = authorizations(&server, app_call).await?;
    assert_eq!(seen.len(), 1, "{app_call}");
    for authorization in seen {
      verify_app_jwt(&authorization)?;
    }
  }
  assert_eq!(hits(&server, REPO_JIT).await?, 1);
  assert_app_registration(home.path(), &repo_config(home.path()))
}

/// AC-3: with `github-app.json` for the host and no personal token given,
/// the App is used even though a stored login exists.
#[tokio::test]
async fn the_saved_app_beats_a_stored_login() -> TestResult {
  let home = tempfile::tempdir()?;
  write_app(home.path(), "127.0.0.1")?;
  store_login(home.path(), "127.0.0.1", "stored-login-token")?;
  let server = app_server().await?;
  let out = register(home.path(), &server, "octo/demo", None, &[])?;
  assert!(out.status.success(), "stderr: {}", text(&out.stderr));
  assert_eq!(hits(&server, TOKENS).await?, 1);
  assert_app_registration(home.path(), &repo_config(home.path()))
}

/// AC-3: `--token` or `TOOLU_RUNNER_TOKEN` keeps the token path: no App
/// call, and the config keeps its pre-#201 shape (no `[auth]`).
#[tokio::test]
async fn a_personal_token_skips_the_app() -> TestResult {
  for (env_token, extra) in [
    (None, vec!["--token", "flag-pat"]),
    (Some("flag-pat"), vec![]),
  ] {
    let home = tempfile::tempdir()?;
    write_app(home.path(), "127.0.0.1")?;
    let server = MockServer::start().await;
    mount_jit(&server, "Bearer flag-pat").await?;
    let out = register(home.path(), &server, "octo/demo", env_token, &extra)?;
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert_eq!(hits(&server, REPO_INSTALLATION).await?, 0);
    assert_eq!(hits(&server, TOKENS).await?, 0);
    let written = std::fs::read_to_string(repo_config(home.path()))?;
    assert!(!written.contains("[auth]"), "{written}");
    assert_eq!(
      load_config(&repo_config(home.path()))?.auth,
      AuthSection::Token
    );
  }
  Ok(())
}

/// AC-4: an App not installed on the target fails naming where it IS
/// installed, the install URL and `--token`; nothing is written.
#[tokio::test]
async fn an_app_not_installed_on_the_repo_fails_with_the_fix() -> TestResult {
  let home = tempfile::tempdir()?;
  write_app(home.path(), "127.0.0.1")?;
  let server = MockServer::start().await;
  Mock::given(method("GET"))
    .and(path(REPO_INSTALLATION))
    .respond_with(ResponseTemplate::new(404).set_body_json(json!({
      "message": "Not Found",
      "documentation_url": "https://docs.github.com/rest/apps/apps#get-a-repository-installation-for-the-authenticated-app",
      "status": "404",
    })))
    .mount(&server)
    .await;
  Mock::given(method("GET"))
    .and(path("/api/v3/app/installations"))
    .respond_with(capture("installations_200.json")?)
    .mount(&server)
    .await;
  let out = register(home.path(), &server, "octo/demo", None, &["--app"])?;
  assert_eq!(out.status.code(), Some(2), "stdout: {}", text(&out.stdout));
  let stderr = text(&out.stderr);
  for expected in [
    "is not installed on octo/demo",
    "it is installed on: octocat",
    INSTALL_URL,
    "--token",
  ] {
    assert!(stderr.contains(expected), "missing {expected:?}: {stderr}");
  }
  assert!(!repo_config(home.path()).exists(), "nothing may be written");
  assert_eq!(hits(&server, TOKENS).await?, 0);
  Ok(())
}

/// AC-5: an org URL looks up the org installation, resolves the runner-group
/// name through the org's list, and mints at the org endpoint with its id.
#[tokio::test]
async fn an_org_registration_resolves_its_runner_group_name() -> TestResult {
  let home = tempfile::tempdir()?;
  write_app(home.path(), "127.0.0.1")?;
  let server = app_server().await?;
  Mock::given(method("GET"))
    .and(path("/api/v3/orgs/octo-org/actions/runner-groups"))
    .and(header(
      "authorization",
      format!("Bearer {INSTALLATION_TOKEN}"),
    ))
    .respond_with(capture("runner_groups_200.json")?)
    .mount(&server)
    .await;
  Mock::given(method("POST"))
    .and(path(ORG_JIT))
    .and(body_partial_json(json!({"runner_group_id": 2})))
    .respond_with(
      ResponseTemplate::new(201).set_body_json(serde_json::from_str::<Value>(JIT_FIXTURE)?),
    )
    .with_priority(1)
    .expect(1)
    .mount(&server)
    .await;
  let out = register(
    home.path(),
    &server,
    "octo-org",
    None,
    &["--app", "--runner-group", "octo-runner-group"],
  )?;
  assert!(out.status.success(), "stderr: {}", text(&out.stderr));
  assert_eq!(hits(&server, ORG_INSTALLATION).await?, 1);
  let config = home.path().join("config.toml");
  assert_eq!(load_config(&config)?.runner_group, "2");
  assert_app_registration(home.path(), &config)
}

/// AC-5: an unknown org runner-group name fails listing the groups.
#[tokio::test]
async fn an_unknown_runner_group_lists_the_groups() -> TestResult {
  let home = tempfile::tempdir()?;
  write_app(home.path(), "127.0.0.1")?;
  let server = app_server().await?;
  Mock::given(method("GET"))
    .and(path("/api/v3/orgs/octo-org/actions/runner-groups"))
    .respond_with(capture("runner_groups_200.json")?)
    .mount(&server)
    .await;
  let out = register(
    home.path(),
    &server,
    "octo-org",
    None,
    &["--app", "--runner-group", "gpu"],
  )?;
  assert_eq!(out.status.code(), Some(2));
  let stderr = text(&out.stderr);
  assert!(
    stderr.contains(
      "runner group 'gpu' not found in octo-org (groups: Default, octo-runner-group, \
       expensive-hardware)"
    ),
    "{stderr}"
  );
  assert_eq!(hits(&server, ORG_JIT).await?, 0);
  Ok(())
}

/// `--app` with no saved App, or an App for another host, fails before any
/// request, naming the fix.
#[tokio::test]
async fn app_without_a_matching_saved_app_fails_before_any_request() -> TestResult {
  let server = MockServer::start().await;
  let home = tempfile::tempdir()?;
  let out = register(home.path(), &server, "octo/demo", None, &["--app"])?;
  assert_eq!(out.status.code(), Some(2));
  assert!(
    text(&out.stderr).contains("toolu-runner create-app"),
    "{}",
    text(&out.stderr)
  );
  write_app(home.path(), "github.com")?;
  let out = register(home.path(), &server, "octo/demo", None, &["--app"])?;
  assert_eq!(out.status.code(), Some(2));
  assert!(
    text(&out.stderr).contains("belongs to github.com, not 127.0.0.1"),
    "{}",
    text(&out.stderr)
  );
  let requests = server.received_requests().await.ok_or("recording")?;
  assert!(requests.is_empty(), "no request may be sent");
  Ok(())
}

/// AC-12: `status` names the App and installation of an App registration.
#[tokio::test]
async fn status_names_the_app_of_an_app_registration() -> TestResult {
  let home = tempfile::tempdir()?;
  let app_file = write_app(home.path(), "127.0.0.1")?;
  let server = app_server().await?;
  let out = register(home.path(), &server, "octo/demo", None, &["--app"])?;
  assert!(out.status.success(), "stderr: {}", text(&out.stderr));
  let status = Command::new(env!("CARGO_BIN_EXE_toolu-runner"))
    .env("TOOLU_RUNNER_HOME", home.path())
    .env("HOME", home.path())
    .env("TOOLU_RUNNER_NO_KEYRING", "1")
    .args(["status", "--config"])
    .arg(repo_config(home.path()))
    .output()?;
  assert!(status.status.success(), "{}", text(&status.stderr));
  let app_file = std::path::absolute(app_file)?;
  let expected = format!(
    "auth:      GitHub App '{APP_SLUG}' (App 424242, installation 1, {})",
    app_file.display()
  );
  assert!(
    text(&status.stdout).contains(&expected),
    "{}",
    text(&status.stdout)
  );
  Ok(())
}
