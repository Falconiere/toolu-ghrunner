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
use wiremock::matchers::{header, method, path as path_matcher};
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
  // The already-built binary, not `cargo run`: no re-resolve, no build-lock
  // contention with the sibling test binaries.
  let mut cmd = Command::new(env!("CARGO_BIN_EXE_toolu-runner"));
  cmd
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

/// With no token from any source, `remove` must FAIL and keep local state.
/// Deleting the persisted `runner_id` + URL while the runner is still
/// registered is exactly the B-002 outcome; a warning would scroll past, so
/// the operator is made to choose (`login`, `--token`, `--skip-unregister`).
#[tokio::test]
async fn no_token_fails_without_touching_local_state() -> TestResult<()> {
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
    !out.status.success(),
    "remove without a token must not silently strand the runner"
  );
  assert!(
    config_path.exists(),
    "config.toml must survive: it holds the only handle on the runner"
  );
  let stderr = String::from_utf8_lossy(&out.stderr);
  for remedy in [
    "login",
    "--token",
    "TOOLU_RUNNER_TOKEN",
    "--skip-unregister",
  ] {
    assert!(
      stderr.contains(remedy),
      "the error must name {remedy}, got: {stderr}"
    );
  }
  Ok(())
}

/// An exported-but-EMPTY `TOOLU_RUNNER_TOKEN` counts as no token, not as a
/// token: sending `Bearer ` only earns a 401 and would turn a removable
/// registration into a hard failure with a misleading auth error.
#[tokio::test]
async fn empty_env_token_is_treated_as_absent() -> TestResult<()> {
  let dir = tempfile::tempdir()?;
  let home = tempfile::tempdir()?;
  let server = MockServer::start().await;
  Mock::given(method("DELETE"))
    .respond_with(ResponseTemplate::new(204))
    .expect(0)
    .mount(&server)
    .await;

  let config_path = seed_registration(dir.path(), &server.uri())?;
  let mut cmd = Command::new(env!("CARGO_BIN_EXE_toolu-runner"));
  cmd
    .arg("remove")
    .arg("--config")
    .arg(&config_path)
    .env("TOOLU_RUNNER_HOME", home.path())
    .env("TOOLU_RUNNER_TOKEN", "   ");
  let out = cmd.output()?;

  assert!(!out.status.success(), "an empty token is not a token");
  let stderr = String::from_utf8_lossy(&out.stderr);
  assert!(
    stderr.contains("no token available"),
    "must report absence, not an auth failure, got: {stderr}"
  );
  Ok(())
}

/// The stored `login` token is the bearer most operators actually rely on,
/// and it drives a destructive DELETE — so it gets its own coverage rather
/// than riding on the `--token` flag every other test here uses.
#[tokio::test]
async fn stored_login_token_authorizes_the_unregister() -> TestResult<()> {
  let dir = tempfile::tempdir()?;
  let home = tempfile::tempdir()?;
  let server = MockServer::start().await;
  Mock::given(method("DELETE"))
    .and(path_matcher(delete_path()))
    .and(header("authorization", "Bearer stored-tok"))
    .respond_with(ResponseTemplate::new(204))
    .expect(1)
    .mount(&server)
    .await;

  let config_path = seed_registration(dir.path(), &server.uri())?;
  // `AuthStore::File` keys by host; the seeded runner_url is the mock server.
  let host = url::Url::parse(&server.uri())?
    .host_str()
    .unwrap_or_default()
    .to_owned();
  std::fs::write(
    home.path().join(format!("token-{host}.json")),
    format!(
      r#"{{"access_token":"stored-tok","scope":"repo","host":"{host}","issued_at":"2026-08-06T00:00:00+00:00"}}"#
    ),
  )?;

  let out = run_remove(&config_path, home.path(), &[])?;

  assert!(
    out.status.success(),
    "the stored login token should authorize the unregister: {}",
    String::from_utf8_lossy(&out.stderr)
  );
  assert!(!config_path.exists(), "config.toml should be gone");
  Ok(())
}

/// A LEFTOVER `.lock` — present, nobody holding it — must not stop the
/// unregister, with or without `--force`. Nothing deletes `.lock` on a
/// normal `run` exit, so this file is the resting state of any machine that
/// has run a job; treating its mere existence as "a run is in flight" would
/// make the unregister unreachable in the ordinary flow.
#[tokio::test]
async fn a_leftover_lock_file_still_unregisters() -> TestResult<()> {
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
  // A lock file nobody holds: PID 0 is never live, and no flock is taken.
  std::fs::write(
    dir.path().join(".lock"),
    r#"{"pid":0,"started_at":"now","config_path":"/tmp/x"}"#,
  )?;

  // No --force: the ordinary invocation must get through.
  let out = run_remove(&config_path, home.path(), &["--token", "tok-1"])?;

  assert!(
    out.status.success(),
    "a leftover lock must not block remove: {}",
    String::from_utf8_lossy(&out.stderr)
  );
  assert!(!config_path.exists(), "config.toml should be gone");
  let stdout = String::from_utf8_lossy(&out.stdout);
  assert!(
    stdout.contains("unregistered on GitHub"),
    "an unheld lock must not suppress the unregister, got: {stdout}"
  );
  Ok(())
}

/// `--force` past a lock whose holder is ALIVE must not unregister: that
/// job renews and reports against this registration, so deleting it on
/// GitHub would break the very run `--force` targets.
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
  // Hold the job lock exactly as a live `run` does — a real advisory flock,
  // not just a file with a live PID in it. `remove` now ACQUIRES the lock to
  // decide, so only a genuine holder counts.
  let held = config::lockfile::acquire(&dir.path().join(".lock"), &config_path)
    .map_err(|e| format!("test should be able to take the lock: {e}"))?;

  let out = run_remove(&config_path, home.path(), &["--force", "--token", "tok-1"])?;
  drop(held);

  assert!(
    out.status.success(),
    "remove --force should succeed: {}",
    String::from_utf8_lossy(&out.stderr)
  );
  assert!(!config_path.exists(), "config.toml should be gone");
  Ok(())
}

/// An ORG-level registration (`runner_url` with no repo segment) has no
/// address on the repo-scoped runners API. `remove` must still clear local
/// state — org registrations removed fine before B-002, and no retry could
/// ever make the URL resolve, so failing here would strand the operator.
#[tokio::test]
async fn org_level_url_removes_locally_without_failing() -> TestResult<()> {
  let dir = tempfile::tempdir()?;
  let home = tempfile::tempdir()?;
  let server = MockServer::start().await;
  Mock::given(method("DELETE"))
    .respond_with(ResponseTemplate::new(204))
    .expect(0)
    .mount(&server)
    .await;

  // No repo segment: `resolve_runners_base` cannot build a runners URL.
  let config_path = seed_registration(dir.path(), &server.uri())?;
  let raw = std::fs::read_to_string(&config_path)?;
  let org_only = raw.replace("/octo-org/octo-repo", "/octo-org");
  std::fs::write(&config_path, org_only)?;

  let out = run_remove(&config_path, home.path(), &["--token", "tok-1"])?;

  assert!(
    out.status.success(),
    "an org-level registration must still remove locally: {}",
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
