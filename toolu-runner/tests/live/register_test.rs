//! Live tests for the `register` flow (AC #1a, #1b).
//!
//! These tests run the `toolu-runner register` binary against a real
//! test repo and verify the persisted `config.toml` and
//! `credentials.json` files match the spec's storage layout.
//!
//! Each test is `#[ignore]`'d so the test harness compiles under
//! `cargo test --features live` without needing a real PAT. Run with
//! `cargo test -p toolu-runner --features live -- --ignored` to
//! execute, with `TOOLU_RUNNER_LIVE_TOKEN` and `TOOLU_RUNNER_LIVE_REPO`
//! set in the environment.

use toolu_runner::config::{
  CredentialsFile, RunnerRegistrationConfig, load_credentials, load_config as load_reg_config,
};

use super::harness::LiveHarness;

/// Skip the test with a clear message if the live env vars are missing.
macro_rules! require_live_env {
  () => {
    if std::env::var("TOOLU_RUNNER_LIVE_TOKEN").is_err()
      || std::env::var("TOOLU_RUNNER_LIVE_REPO").is_err()
    {
      eprintln!(
        "skipping live test: set TOOLU_RUNNER_LIVE_TOKEN and TOOLU_RUNNER_LIVE_REPO to run"
      );
      return;
    }
  };
}

#[tokio::test]
#[ignore = "live test — requires TOOLU_RUNNER_LIVE_TOKEN"]
async fn register_creates_config_and_credentials() {
  require_live_env!();
  let harness = LiveHarness::new().await.expect("harness init");

  // Register against the test repo. The harness always passes
  // `--replace` so re-runs are idempotent.
  harness.register().await.expect("register");

  // AC #1a: config.toml exists and parses with the expected fields.
  let cfg_path = harness.config_path();
  let creds_path = harness.credentials_path();
  assert!(
    cfg_path.exists(),
    "config.toml was not written to {}",
    cfg_path.display()
  );
  assert!(
    creds_path.exists(),
    "credentials.json was not written to {}",
    creds_path.display()
  );

  let cfg: RunnerRegistrationConfig =
    load_reg_config(&cfg_path).map_err(|e| e.to_string()).expect("load config.toml");
  assert_eq!(
    cfg.runner_url,
    format!("https://github.com/{}", harness.repo),
    "runner_url should be the test repo URL"
  );
  assert!(
    cfg.runner_name.starts_with("toolu-runner-live-"),
    "runner_name should be derived from the repo: {}",
    cfg.runner_name
  );
  assert!(
    cfg.labels.contains(&"self-hosted".to_owned()),
    "labels should include 'self-hosted': {:?}",
    cfg.labels
  );
  assert!(
    cfg.labels.contains(&"toolu-runner-v1".to_owned()),
    "labels should include 'toolu-runner-v1' so workflow `runs-on` matches: {:?}",
    cfg.labels
  );
  assert_eq!(
    cfg.runtime.protocol_version, "v2",
    "github.com should pick v2 protocol; got {}",
    cfg.runtime.protocol_version
  );

  // AC #1b: credentials.json has a non-empty placeholder token and
  // an RFC3339 issued_at. The live JWT-exchange flow lands in step 10.
  let creds: CredentialsFile =
    load_credentials(&creds_path).map_err(|e| e.to_string()).expect("load credentials.json");
  assert!(
    !creds.access_token.is_empty(),
    "access_token should be a non-empty placeholder (live flow is step 10)"
  );
  assert!(
    creds.access_token.starts_with("ghs_placeholder_") || creds.access_token.starts_with("ghs_"),
    "access_token should look like a ghs_ token; got prefix: {}",
    creds.access_token.get(..16).unwrap_or("?")
  );
  assert!(
    !creds.issued_at.is_empty(),
    "issued_at should be an RFC3339 timestamp"
  );

  // Teardown.
  let _ = harness.remove().await;
}

#[tokio::test]
#[ignore = "live test — requires TOOLU_RUNNER_LIVE_TOKEN"]
async fn register_replace_overwrites_existing() {
  require_live_env!();
  let harness = LiveHarness::new().await.expect("harness init");

  // First registration.
  harness.register().await.expect("first register");
  let first_mtime = std::fs::metadata(harness.config_path())
    .and_then(|m| m.modified())
    .ok();

  // Second registration with --replace (which the harness always passes).
  // Should succeed and overwrite the first. Without --replace, the
  // second call would exit 2 with "registration already exists".
  tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
  harness.register().await.expect("second register with --replace");

  assert!(
    harness.config_path().exists(),
    "config.toml should still exist after replace"
  );
  let second_mtime = std::fs::metadata(harness.config_path())
    .and_then(|m| m.modified())
    .ok();
  if let (Some(a), Some(b)) = (first_mtime, second_mtime) {
    assert!(
      b > a,
      "config.toml mtime should advance after replace (first={a:?}, second={b:?})"
    );
  }
  // The configs should both be valid TOML and parse successfully.
  let _: RunnerRegistrationConfig = load_reg_config(&harness.config_path())
    .map_err(|e| e.to_string())
    .expect("second config parses");

  // Teardown.
  let _ = harness.remove().await;
}
