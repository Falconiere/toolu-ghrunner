//! CLI-level tests for `remove`'s GitHub unregister (B-002).
//!
//! The unit tests in `gh_compat_register.rs` cover
//! `wire::net::unregister_runner` itself; these pin the part only the CLI
//! decides — the ORDER. Local state must survive a failed unregister so the
//! removal is retryable, and must not survive a successful one.
//!
//! Real binary, real HTTP: shells out to `cargo run -- remove` against a
//! local `wiremock` standing in for GitHub, with `TOOLU_RUNNER_HOME` and the
//! config's `runner_url` pointed at it. No mocking of internal types.

use std::path::{Path, PathBuf};
use std::process::Command;

use config::config::{
  CacheSection, RunnerRegistrationConfig, RuntimeConfig, ServicesSection, ShadowSection,
  WorkspaceSection, save_config,
};
use wiremock::matchers::{method, path as path_matcher};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Boxed error alias so helpers can use `?`.
type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

/// The runner id the seeded config persists — the id `remove` must DELETE.
const RUNNER_ID: i64 = 461;

/// Seed a registration whose `runner_url` points at `server`, so the
/// binary's unregister call lands on the local stub. Returns the config path.
fn seed_registration(dir: &Path, server_uri: &str) -> TestResult<PathBuf> {
  let config_path = dir.join("config.toml");
  let config = RunnerRegistrationConfig {
    runner_url: format!("{server_uri}/octo-org/octo-repo"),
    runner_name: "runner-1".to_owned(),
    runner_id: RUNNER_ID,
    auth_token: "fixture-client-id".to_owned(),
    labels: vec!["self-hosted".to_owned()],
    runner_group: "Default".to_owned(),
    runtime: RuntimeConfig {
      jit_config: "fixture-jit-blob".to_owned(),
      work_dir: "_work".to_owned(),
      data_dir: dir.to_string_lossy().into_owned(),
      protocol_version: "v2".to_owned(),
    },
    services: ServicesSection::default(),
    cache: CacheSection::default(),
    workspace: WorkspaceSection::default(),
    shadow: ShadowSection::default(),
  };
  save_config(&config_path, &config)?;
  std::fs::write(dir.join("credentials.json"), "{}")?;
  Ok(config_path)
}

/// The api/v3 DELETE path derived for `octo-org/octo-repo`.
fn delete_path() -> String {
  format!("/api/v3/repos/octo-org/octo-repo/actions/runners/{RUNNER_ID}")
}

/// Run `remove` against the seeded registration. `home` isolates the token
/// store so no developer's real `login` token leaks into the test.
fn run_remove(config_path: &Path, home: &Path, extra: &[&str]) -> TestResult<std::process::Output> {
  let mut cmd = Command::new(env!("CARGO"));
  cmd
    .args(["run", "-p", "toolu-runner", "--quiet", "--"])
    .arg("remove")
    .arg("--config")
    .arg(config_path)
    .args(extra)
    .env("TOOLU_RUNNER_HOME", home)
    .env_remove("TOOLU_RUNNER_TOKEN");
  Ok(cmd.output()?)
}

/// A successful unregister deletes local state — and really does call the
/// DELETE (`expect(1)` fails the test on drop otherwise).
#[tokio::test]
async fn successful_unregister_then_deletes_local_state() -> TestResult<()> {
  let dir = tempfile::tempdir()?;
  let home = tempfile::tempdir()?;
  let server = MockServer::start().await;
  Mock::given(method("DELETE"))
    .and(path_matcher(delete_path()))
    .respond_with(ResponseTemplate::new(204))
    .expect(1)
    .mount(&server)
    .await;

  let config_path = seed_registration(dir.path(), &server.uri())?;
  let out = run_remove(&config_path, home.path(), &["--token", "tok-1"])?;

  assert!(
    out.status.success(),
    "remove should succeed: {}",
    String::from_utf8_lossy(&out.stderr)
  );
  assert!(
    !config_path.exists(),
    "config.toml should be gone after a successful unregister"
  );
  Ok(())
}

/// THE ordering guarantee: a failed unregister must leave local state
/// untouched. The persisted `runner_id` and URL are the only handle on the
/// GitHub-side runner, so deleting them here would strand it — the exact
/// failure B-002 is about, just moved one step later.
#[tokio::test]
async fn failed_unregister_keeps_local_state_for_retry() -> TestResult<()> {
  let dir = tempfile::tempdir()?;
  let home = tempfile::tempdir()?;
  let server = MockServer::start().await;
  Mock::given(method("DELETE"))
    .and(path_matcher(delete_path()))
    .respond_with(ResponseTemplate::new(401).set_body_string(r#"{"message":"Bad credentials"}"#))
    .expect(1)
    .mount(&server)
    .await;

  let config_path = seed_registration(dir.path(), &server.uri())?;
  let creds_path = dir.path().join("credentials.json");
  let out = run_remove(&config_path, home.path(), &["--token", "bad-token"])?;

  assert!(
    !out.status.success(),
    "remove must fail when the unregister fails"
  );
  assert!(
    config_path.exists(),
    "config.toml must survive a failed unregister so the removal can be retried"
  );
  assert!(
    creds_path.exists(),
    "credentials.json must survive a failed unregister"
  );
  let stderr = String::from_utf8_lossy(&out.stderr);
  assert!(
    stderr.contains("--skip-unregister"),
    "the error should name the escape hatch, got: {stderr}"
  );
  Ok(())
}

/// `--skip-unregister` removes local state and makes NO request at all —
/// `expect(0)` catches an attempted DELETE regardless of how it is answered.
#[tokio::test]
async fn skip_unregister_deletes_locally_without_calling_github() -> TestResult<()> {
  let dir = tempfile::tempdir()?;
  let home = tempfile::tempdir()?;
  let server = MockServer::start().await;
  Mock::given(method("DELETE"))
    .respond_with(ResponseTemplate::new(204))
    .expect(0)
    .mount(&server)
    .await;

  let config_path = seed_registration(dir.path(), &server.uri())?;
  let out = run_remove(
    &config_path,
    home.path(),
    &["--skip-unregister", "--token", "tok-1"],
  )?;

  assert!(
    out.status.success(),
    "remove --skip-unregister should succeed: {}",
    String::from_utf8_lossy(&out.stderr)
  );
  assert!(!config_path.exists(), "config.toml should be gone");
  Ok(())
}

/// With no token from any source, the local removal still proceeds — it
/// worked before this feature and must keep working unauthenticated — but
/// no request is made and the output says the runner was left registered.
#[tokio::test]
async fn no_token_removes_locally_and_says_so() -> TestResult<()> {
  let dir = tempfile::tempdir()?;
  let home = tempfile::tempdir()?;
  let server = MockServer::start().await;
  Mock::given(method("DELETE"))
    .respond_with(ResponseTemplate::new(204))
    .expect(0)
    .mount(&server)
    .await;

  let config_path = seed_registration(dir.path(), &server.uri())?;
  let out = run_remove(&config_path, home.path(), &[])?;

  assert!(
    out.status.success(),
    "remove without a token should still clear local state: {}",
    String::from_utf8_lossy(&out.stderr)
  );
  assert!(!config_path.exists(), "config.toml should be gone");
  let stdout = String::from_utf8_lossy(&out.stdout);
  assert!(
    stdout.contains("still registered on GitHub"),
    "output must say the runner was left registered, got: {stdout}"
  );
  Ok(())
}

/// `--force` past a HELD lock must not unregister: the job still running
/// renews and reports against this registration, so deleting it on GitHub
/// would break the very run `--force` targets.
#[tokio::test]
async fn force_past_an_in_flight_run_does_not_unregister() -> TestResult<()> {
  let dir = tempfile::tempdir()?;
  let home = tempfile::tempdir()?;
  let server = MockServer::start().await;
  Mock::given(method("DELETE"))
    .respond_with(ResponseTemplate::new(204))
    .expect(0)
    .mount(&server)
    .await;

  let config_path = seed_registration(dir.path(), &server.uri())?;
  // A held job lock, as `run` leaves it.
  std::fs::write(dir.path().join(".lock"), r#"{"pid":1,"started_at":"now"}"#)?;

  let out = run_remove(&config_path, home.path(), &["--force", "--token", "tok-1"])?;

  assert!(
    out.status.success(),
    "remove --force should succeed: {}",
    String::from_utf8_lossy(&out.stderr)
  );
  assert!(!config_path.exists(), "config.toml should be gone");
  Ok(())
}
