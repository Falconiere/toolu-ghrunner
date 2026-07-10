//! Live login E2E: `register` reuses a stored token (AC-2), a bad stored
//! token maps to the login hint (AC-11), and the full device flow stores
//! a token (AC-1, manual).
//!
//! Mirrors `tests/live_e2e.rs`: gated behind `#![cfg(feature = "live")]`,
//! reuses the shared `LiveHarness`, the `require_live_env!` gate, and the
//! `TOOLU_RUNNER_LIVE_TOKEN` / `TOOLU_RUNNER_LIVE_REPO` env contract.
//!
//! These compile under `cargo test --features live --no-run` with no live
//! GitHub. To actually run them you need a real test repo + PAT (AC-2 /
//! AC-11) or a real OAuth App `client_id` + browser (AC-1); each is
//! `#[ignore]`d and executed with `-- --ignored`.
//!
//! The stored-token path assumes a keyless environment (temp HOME, no OS
//! keyring) so `AuthStore::new` inside `register` resolves to the same
//! `File` backend these tests seed. That is how the Linux CI runner runs.

#![cfg(feature = "live")]

use std::process::{Output, Stdio};

use tokio::process::Command;
use toolu_runner::auth_store::{AuthStore, StoredToken};

#[path = "helpers/live_harness.rs"]
mod harness;

use harness::LiveHarness;

/// Skip the test with a clear message if the live env vars are missing.
macro_rules! require_live_env {
  () => {
    if std::env::var("TOOLU_RUNNER_LIVE_TOKEN").is_err()
      || std::env::var("TOOLU_RUNNER_LIVE_REPO").is_err()
    {
      eprintln!(
        "skipping live login test: set TOOLU_RUNNER_LIVE_TOKEN and TOOLU_RUNNER_LIVE_REPO to run"
      );
      return;
    }
  };
}

/// Seed the File-backed token store next to the harness's `config.toml`
/// with `access_token` for github.com. `register` (invoked without
/// `--token`) must discover and reuse it.
fn seed_stored_token(harness: &LiveHarness, access_token: &str) {
  let store = AuthStore::File(harness.config_dir.path().to_path_buf());
  store
    .save(&StoredToken {
      access_token: access_token.to_owned(),
      scope: "repo".to_owned(),
      host: "github.com".to_owned(),
      issued_at: "2026-07-10T00:00:00+00:00".to_owned(),
    })
    .expect("seed stored token");
}

/// Invoke `toolu-runner register --url <repo>` WITHOUT `--token`, so the
/// REST bearer must come from the stored login token. Returns the
/// captured process output.
async fn register_without_token(harness: &LiveHarness) -> Output {
  let url = format!("https://github.com/{}", harness.repo);
  let runner_name = format!(
    "toolu-runner-live-{}",
    harness.repo.replace('/', "-").to_lowercase()
  );
  let config_path = harness.config_path().to_string_lossy().into_owned();
  let work_path = harness.work_dir.to_string_lossy().into_owned();
  Command::new(&harness.binary_path)
    .args([
      "register",
      "--url",
      &url,
      "--name",
      &runner_name,
      "--labels",
      "self-hosted,toolu-runner-v1,linux,x64",
      "--config",
      &config_path,
      "--work",
      &work_path,
      "--replace",
    ])
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .output()
    .await
    .expect("spawn register")
}

/// AC-2: `register` with no `--token` reuses the stored login token and
/// mints a JIT config, exactly as the explicit-PAT flow does.
#[tokio::test]
#[ignore = "live test — requires TOOLU_RUNNER_LIVE_TOKEN + keyless File store"]
async fn register_reuses_stored_token() {
  require_live_env!();
  let harness = LiveHarness::new().await.expect("harness init");
  let _cleanup = harness.cleanup(&[]).await;

  // Seed with the real PAT, then register with NO --token.
  seed_stored_token(&harness, &harness.token);
  let output = register_without_token(&harness).await;

  assert!(
    output.status.success(),
    "register with a stored token should mint a JIT; stderr: {}",
    String::from_utf8_lossy(&output.stderr)
  );
  assert!(
    harness.credentials_path().exists(),
    "JIT credentials.json should be written from the stored-token register"
  );

  let _ = harness.remove().await;
}

/// AC-11: a deliberately-bogus stored token yields a `generate-jitconfig`
/// 401, which the runner maps to a non-zero exit and a "run
/// 'toolu-runner login'" hint on stderr.
#[tokio::test]
#[ignore = "live test — requires TOOLU_RUNNER_LIVE_TOKEN + keyless File store"]
async fn register_bad_stored_token_maps_to_login_hint() {
  require_live_env!();
  let harness = LiveHarness::new().await.expect("harness init");
  let _cleanup = harness.cleanup(&[]).await;

  seed_stored_token(&harness, "ghp_deadbeefdeadbeefdeadbeefdeadbeefdead");
  let output = register_without_token(&harness).await;

  assert!(
    !output.status.success(),
    "register with a bogus token must fail (non-zero exit)"
  );
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(
    stderr.contains("run 'toolu-runner login'"),
    "a 401 should map to the login hint; stderr: {stderr}"
  );
}

/// AC-1: full device-flow login, end to end.
///
/// MANUAL: this needs a real github.com OAuth App `client_id` (the
/// built-in `DEVICE_CLIENT_ID` is still `REPLACE_ME`) and interactive
/// browser approval, so it cannot run unattended. Run it by hand:
///
/// ```text
/// cargo test -p toolu-runner --features live --test live_login_e2e \
///   device_flow_login_stores_token -- --ignored --nocapture
/// ```
///
/// then open the printed URL, enter the code, and approve. On return the
/// token must be readable from the store. (`LiveHarness::new` still needs
/// TOOLU_RUNNER_LIVE_TOKEN/REPO set to build the binary, even though the
/// login flow itself uses neither.)
#[tokio::test]
#[ignore = "manual: interactive device-flow login, needs a real OAuth App client_id"]
async fn device_flow_login_stores_token() {
  require_live_env!();
  let harness = LiveHarness::new().await.expect("harness init");
  let config_path = harness.config_path().to_string_lossy().into_owned();

  // Drive the real `login` command: prints a code, opens the browser,
  // polls until the user approves, then persists the token.
  let status = Command::new(&harness.binary_path)
    .args(["login", "github.com", "--config", &config_path])
    .status()
    .await
    .expect("spawn login");
  assert!(status.success(), "login should exit 0 after approval");

  // The token must now be readable from the store next to config.toml.
  let store = AuthStore::File(harness.config_dir.path().to_path_buf());
  let stored = store
    .load("github.com")
    .expect("load stored token")
    .expect("a token must be stored after a successful login");
  assert!(
    !stored.access_token.is_empty(),
    "stored token must be non-empty"
  );
}
