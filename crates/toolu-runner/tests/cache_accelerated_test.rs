//! S12 — accelerated services mode env injection, driven through the real
//! `run_job` assembly path against the committed real-data `job_message.json`.
//!
//! Real-data only, no mocks. A single script step dumps its `ACTIONS_*` env to
//! a file (not stdout, so the secret masker — which rewrites only log lines —
//! never redacts the runtime token). The test then asserts:
//!   - cache traffic is redirected local: `ACTIONS_RESULTS_URL` and
//!     `ACTIONS_CACHE_URL` both point at `http://127.0.0.1:<port>`, NOT the
//!     fixture's real results URL, and `ACTIONS_CACHE_SERVICE_V2=true`;
//!   - non-cache vars are still forwarded to real GitHub: `ACTIONS_RUNTIME_URL`
//!     is the fixture's pipelines URL and `ACTIONS_RUNTIME_TOKEN` is the real,
//!     unchanged fixture token (the proxy forwards it upstream for artifacts).

use std::collections::HashMap;
use std::error::Error;
use std::sync::{Arc, Mutex};

use shared::SecretMasker;
use shared::{ActionStep, AgentJobRequestMessage, RunnerConfig, RunnerEvent, ServicesMode};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use execution::execution::job_runner::run_job;

const JOB_MESSAGE: &str = include_str!("fixtures/job_message.json");

// Fixture values (must mirror tests/fixtures/job_message.json).
const FIX_RESULTS: &str = "https://results-receiver.actions.githubusercontent.com/";
const FIX_PIPELINES: &str =
  "https://pipelines.actions.githubusercontent.com/aBcDeFgHiJkLmNoPqRsTuVwXyZ0123456789/";
const FIX_TOKEN: &str = "eyJ0eXAiOiJKV1QiLCJhbGciOiJSUzI1NiJ9.EXAMPLE.PAYLOAD";

const LOCAL_PREFIX: &str = "http://127.0.0.1:";

type TestResult<T> = Result<T, Box<dyn Error>>;

fn fixture_job() -> TestResult<AgentJobRequestMessage> {
  Ok(serde_json::from_str(JOB_MESSAGE)?)
}

/// Run a real accelerated job whose single step writes its `ACTIONS_*` env to a
/// file, then parse that file into a map.
///
/// The env is dumped to a file rather than stdout precisely so the runtime
/// token is observed unredacted: the `SecretMasker` rewrites `Log` events, not
/// a step's own file writes.
async fn accelerated_env() -> TestResult<HashMap<String, String>> {
  let dir = tempfile::tempdir()?;
  let workspace_root = dir.path().join("work");
  let data_dir = dir.path().join("data");
  std::fs::create_dir_all(&workspace_root)?;
  std::fs::create_dir_all(&data_dir)?;
  let dump = dir.path().join("env-dump.txt");

  let config = RunnerConfig {
    data_dir,
    workspace_root,
    cgroup_path: None,
    services_mode: ServicesMode::Accelerated,
    ..RunnerConfig::default()
  };

  let script = format!(
    "printf 'ACTIONS_RESULTS_URL=%s\\nACTIONS_CACHE_URL=%s\\nACTIONS_CACHE_SERVICE_V2=%s\\nACTIONS_RUNTIME_TOKEN=%s\\nACTIONS_RUNTIME_URL=%s\\n' \"$ACTIONS_RESULTS_URL\" \"$ACTIONS_CACHE_URL\" \"$ACTIONS_CACHE_SERVICE_V2\" \"$ACTIONS_RUNTIME_TOKEN\" \"$ACTIONS_RUNTIME_URL\" > '{}'",
    dump.display()
  );

  let mut msg = fixture_job()?;
  msg.steps = vec![ActionStep::script("dump-env", &script, "")];

  let masker = Arc::new(Mutex::new(SecretMasker::new()));
  let (tx, mut rx) = mpsc::channel::<RunnerEvent>(1024);
  let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });

  run_job(msg, &config, CancellationToken::new(), tx, masker).await?;
  drain.await?;

  let text = std::fs::read_to_string(&dump)?;
  Ok(parse_env(&text))
}

/// Parse `KEY=VALUE` lines into a map (the first `=` splits key from value).
fn parse_env(text: &str) -> HashMap<String, String> {
  text
    .lines()
    .filter_map(|line| line.split_once('='))
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect()
}

#[tokio::test]
async fn accelerated_redirects_cache_vars_and_keeps_real_token() -> TestResult<()> {
  let env = accelerated_env().await?;

  // Cache traffic is redirected at the local server (loopback), not real GitHub.
  assert!(
    env
      .get("ACTIONS_RESULTS_URL")
      .is_some_and(|v| v.starts_with(LOCAL_PREFIX)),
    "ACTIONS_RESULTS_URL should point at the local server; got {:?}",
    env.get("ACTIONS_RESULTS_URL")
  );
  assert_ne!(
    env.get("ACTIONS_RESULTS_URL").map(String::as_str),
    Some(FIX_RESULTS),
    "ACTIONS_RESULTS_URL must NOT be the fixture's real results URL"
  );
  assert!(
    env
      .get("ACTIONS_CACHE_URL")
      .is_some_and(|v| v.starts_with(LOCAL_PREFIX)),
    "ACTIONS_CACHE_URL should point at the local server; got {:?}",
    env.get("ACTIONS_CACHE_URL")
  );
  assert_eq!(
    env.get("ACTIONS_CACHE_SERVICE_V2").map(String::as_str),
    Some("true"),
    "ACTIONS_CACHE_SERVICE_V2 must be set so modern clients prefer v2"
  );

  // Non-cache vars still reach real GitHub: runtime URL forwarded, token intact.
  assert_eq!(
    env.get("ACTIONS_RUNTIME_URL").map(String::as_str),
    Some(FIX_PIPELINES),
    "ACTIONS_RUNTIME_URL must still be forwarded to real GitHub"
  );
  assert_eq!(
    env.get("ACTIONS_RUNTIME_TOKEN").map(String::as_str),
    Some(FIX_TOKEN),
    "ACTIONS_RUNTIME_TOKEN must stay the real, unchanged GitHub token"
  );
  Ok(())
}
