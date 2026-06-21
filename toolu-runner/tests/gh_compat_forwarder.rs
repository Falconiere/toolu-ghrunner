//! AC-10/11/12/13 (E3, S14/S15) — forwarder service-URL injection + offline
//! mode, exercised against the committed real-data `job_message.json` fixture.
//!
//! Real-data only, no mocks. The unit checks read the fixture's
//! `SystemVssConnection` endpoint through the same `extract_service_urls` /
//! `forward_env` the runner uses; the end-to-end checks drive the real job
//! assembly path (`run_job` → `setup_job_env` → step env) with a script step
//! that echoes the injected `ACTIONS_*` vars.
//!
//! Asserts:
//!   1. `extract_service_urls(fixture)` returns the fixture's
//!      Results/Cache/Pipelines/IdToken URLs + the OAuth `AccessToken`.
//!   2. `forward_env` emits all six `ACTIONS_*` vars with the fixture values;
//!      a `None` URL field OMITS its var; `cache_service_v2=false` omits
//!      `ACTIONS_CACHE_SERVICE_V2`, `true` emits it.
//!   3. Forwarder mode: a step's env (built through the real path) carries
//!      `ACTIONS_RESULTS_URL` from the message — the hermetic portion of
//!      AC-10/11/12 (the live `@v4` round-trip is S16).
//!   4. AC-13 offline mode: a step's `ACTIONS_CACHE_URL` points at the local
//!      cache service (`http://127.0.0.1:…`), not the message URL.

use std::error::Error;
use std::sync::{Arc, Mutex};

use shared::{
  ActionStep, AgentJobRequestMessage, LogStream, RunnerConfig, RunnerEvent, ServicesMode,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use toolu_runner::execution::job_runner::run_job;
use toolu_runner::execution::secret_masker::SecretMasker;
use toolu_runner::execution::service_endpoints::{ServiceUrls, extract_service_urls, forward_env};

const JOB_MESSAGE: &str = include_str!("fixtures/job_message.json");

// Fixture values (must mirror tests/fixtures/job_message.json).
const FIX_RESULTS: &str = "https://results-receiver.actions.githubusercontent.com/";
const FIX_CACHE: &str =
  "https://acghubeus2.actions.githubusercontent.com/aBcDeFgHiJkLmNoPqRsTuVwXyZ0123456789/";
const FIX_PIPELINES: &str =
  "https://pipelines.actions.githubusercontent.com/aBcDeFgHiJkLmNoPqRsTuVwXyZ0123456789/";
const FIX_TOKEN: &str = "eyJ0eXAiOiJKV1QiLCJhbGciOiJSUzI1NiJ9.EXAMPLE.PAYLOAD";

type TestResult<T> = Result<T, Box<dyn Error>>;

fn fixture_job() -> TestResult<AgentJobRequestMessage> {
  Ok(serde_json::from_str(JOB_MESSAGE)?)
}

/// Build a `ServiceUrls` with all fields present (for `forward_env` coverage).
fn full_urls() -> ServiceUrls {
  ServiceUrls {
    results_url: Some(FIX_RESULTS.to_owned()),
    runtime_url: Some(FIX_PIPELINES.to_owned()),
    cache_url: Some(FIX_CACHE.to_owned()),
    cache_service_v2: false,
    id_token_request_url: Some("https://idtoken.example/".to_owned()),
    runtime_token: FIX_TOKEN.to_owned(),
  }
}

#[test]
fn extract_reads_system_connection_urls_and_token() -> TestResult<()> {
  let msg = fixture_job()?;
  let urls = extract_service_urls(&msg);

  assert_eq!(urls.results_url.as_deref(), Some(FIX_RESULTS));
  assert_eq!(urls.cache_url.as_deref(), Some(FIX_CACHE));
  assert_eq!(urls.runtime_url.as_deref(), Some(FIX_PIPELINES));
  assert!(
    urls
      .id_token_request_url
      .as_deref()
      .is_some_and(|u| u.ends_with("/idtoken")),
    "GenerateIdTokenUrl should resolve id_token_request_url, got {:?}",
    urls.id_token_request_url
  );
  assert_eq!(urls.runtime_token, FIX_TOKEN);
  // The fixture carries no ACTIONS_CACHE_SERVICE_V2 variable → false.
  assert!(!urls.cache_service_v2);
  Ok(())
}

#[test]
fn forward_env_emits_all_vars_with_fixture_values() -> TestResult<()> {
  let env: std::collections::HashMap<String, String> =
    forward_env(&extract_service_urls(&fixture_job()?))
      .into_iter()
      .collect();

  assert_eq!(
    env.get("ACTIONS_RESULTS_URL").map(String::as_str),
    Some(FIX_RESULTS)
  );
  assert_eq!(
    env.get("ACTIONS_CACHE_URL").map(String::as_str),
    Some(FIX_CACHE)
  );
  assert_eq!(
    env.get("ACTIONS_RUNTIME_URL").map(String::as_str),
    Some(FIX_PIPELINES)
  );
  assert_eq!(
    env.get("ACTIONS_RUNTIME_TOKEN").map(String::as_str),
    Some(FIX_TOKEN)
  );
  // The runtime token is reused for the id-token request token.
  assert_eq!(
    env
      .get("ACTIONS_ID_TOKEN_REQUEST_TOKEN")
      .map(String::as_str),
    Some(FIX_TOKEN)
  );
  assert!(env.contains_key("ACTIONS_ID_TOKEN_REQUEST_URL"));
  // cache_service_v2 is false in the fixture → the var is omitted.
  assert!(!env.contains_key("ACTIONS_CACHE_SERVICE_V2"));
  Ok(())
}

#[test]
fn forward_env_omits_none_fields_never_empty() {
  let mut urls = full_urls();
  urls.cache_url = None;
  urls.id_token_request_url = None;

  let env: std::collections::HashMap<String, String> = forward_env(&urls).into_iter().collect();

  // Present fields are emitted; absent fields are OMITTED (not empty strings).
  assert!(env.contains_key("ACTIONS_RESULTS_URL"));
  assert!(env.contains_key("ACTIONS_RUNTIME_URL"));
  assert!(!env.contains_key("ACTIONS_CACHE_URL"));
  assert!(!env.contains_key("ACTIONS_ID_TOKEN_REQUEST_URL"));
  // No emitted var is ever an empty value.
  assert!(env.values().all(|v| !v.is_empty()));
}

#[test]
fn forward_env_cache_v2_only_when_true() {
  let mut urls = full_urls();

  let off: std::collections::HashMap<String, String> = forward_env(&urls).into_iter().collect();
  assert!(!off.contains_key("ACTIONS_CACHE_SERVICE_V2"));

  urls.cache_service_v2 = true;
  let on: std::collections::HashMap<String, String> = forward_env(&urls).into_iter().collect();
  assert_eq!(
    on.get("ACTIONS_CACHE_SERVICE_V2").map(String::as_str),
    Some("true")
  );
}

/// Run a single echo step through the full `run_job` path and return the
/// concatenated stdout (`Log`) lines.
async fn run_echo_job(body: &str, mode: ServicesMode) -> TestResult<String> {
  let dir = tempfile::tempdir()?;
  let workspace_root = dir.path().join("work");
  let data_dir = dir.path().join("data");
  std::fs::create_dir_all(&workspace_root)?;
  std::fs::create_dir_all(&data_dir)?;

  let config = RunnerConfig {
    data_dir,
    workspace_root,
    cgroup_path: None,
    services_mode: mode,
  };

  let mut msg = fixture_job()?;
  msg.steps = vec![ActionStep::script("echo-env", body, "")];

  let masker = Arc::new(Mutex::new(SecretMasker::new()));
  let (tx, mut rx) = mpsc::channel::<RunnerEvent>(1024);
  let collector = tokio::spawn(async move {
    let mut lines = String::new();
    while let Some(ev) = rx.recv().await {
      if let RunnerEvent::Log {
        line,
        stream: LogStream::Stdout,
        ..
      } = ev
      {
        lines.push_str(&line);
        lines.push('\n');
      }
    }
    lines
  });

  run_job(msg, &config, CancellationToken::new(), tx, masker).await?;
  Ok(collector.await?)
}

#[tokio::test]
async fn forwarder_injects_results_url_into_step_env() -> TestResult<()> {
  // Default forwarder mode: the step inherits ACTIONS_RESULTS_URL from the
  // message via the real assembly path (run_job → setup_job_env → step env).
  let out = run_echo_job(
    "printf 'RESULTS=%s\\n' \"$ACTIONS_RESULTS_URL\"",
    ServicesMode::Forwarder,
  )
  .await?;
  assert!(
    out.contains(&format!("RESULTS={FIX_RESULTS}")),
    "forwarder step env missing ACTIONS_RESULTS_URL; got:\n{out}"
  );
  Ok(())
}

#[tokio::test]
async fn offline_points_cache_url_at_local_service() -> TestResult<()> {
  // AC-13: offline mode hosts the local cache service and wires the step's
  // ACTIONS_CACHE_URL at it (a loopback address), not the message URL.
  let out = run_echo_job(
    "printf 'CACHE=%s\\n' \"$ACTIONS_CACHE_URL\"",
    ServicesMode::Offline,
  )
  .await?;
  assert!(
    out.contains("CACHE=http://127.0.0.1:"),
    "offline step env should point ACTIONS_CACHE_URL at the local service; got:\n{out}"
  );
  assert!(
    !out.contains(FIX_CACHE),
    "offline mode must NOT forward the real cache URL; got:\n{out}"
  );
  Ok(())
}
