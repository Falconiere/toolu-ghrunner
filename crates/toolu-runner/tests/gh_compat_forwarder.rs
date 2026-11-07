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
//!   2. `forward_env` hands each step type its official set: Node (all vars,
//!      the v2 flag as `True`, the cache mode), container (no v2 flag) and
//!      `run:` (OIDC pair only); a `None` URL omits its var; the flag parses
//!      case-insensitively as boolean true.
//!   3. Through `run_job`, a `run:` step sees only the OIDC pair, and offline
//!      mode gives it no cache vars. Through `run_steps` with service env
//!      installed, a Node action sees the full set and saves/restores real bytes.

use std::error::Error;
use std::sync::{Arc, Mutex};

use execution::execution::job_runner::run_job;
use execution::execution::service_endpoints::{
  ServiceEnv, ServiceUrls, extract_service_urls, forward_env,
};
use shared::SecretMasker;
use shared::{
  ActionStep, AgentJobRequestMessage, LogStream, RunnerConfig, RunnerEvent, ServicesMode,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

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

type EnvMap = std::collections::HashMap<String, String>;

fn map(pairs: Vec<(String, String)>) -> EnvMap {
  pairs.into_iter().collect()
}

/// One shared client for every probe invocation in this file.
fn shared_client() -> &'static reqwest::Client {
  static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
  CLIENT.get_or_init(reqwest::Client::new)
}

/// Build a `ServiceUrls` with all fields present (for selector coverage).
fn full_urls() -> ServiceUrls {
  ServiceUrls {
    results_url: Some(FIX_RESULTS.to_owned()),
    runtime_url: Some(FIX_PIPELINES.to_owned()),
    cache_url: Some(FIX_CACHE.to_owned()),
    cache_service_v2: false,
    cache_mode: None,
    id_token_request_url: Some("https://idtoken.example/".to_owned()),
    runtime_token: FIX_TOKEN.to_owned(),
  }
}

/// The #103 acquired message, with the cache variables replaced.
fn compat_job(
  flag: Option<(&str, &str)>,
  mode: Option<&str>,
) -> TestResult<AgentJobRequestMessage> {
  let mut msg: AgentJobRequestMessage = serde_json::from_str(include_str!(
    "../../execution/tests/compat_103_contexts_message.json"
  ))?;
  msg.variables.retain(|key, _| {
    !key.eq_ignore_ascii_case("actions_uses_cache_service_v2")
      && !key.eq_ignore_ascii_case("actions_cache_mode")
  });
  let var = |value: &str| shared::VariableValue {
    value: value.to_owned(),
    is_secret: false,
  };
  if let Some((key, value)) = flag {
    msg.variables.insert(key.to_owned(), var(value));
  }
  if let Some(mode) = mode {
    msg
      .variables
      .insert("actions_cache_mode".to_owned(), var(mode));
  }
  Ok(msg)
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
  assert!(!urls.cache_service_v2);
  assert_eq!(urls.cache_mode, None);
  Ok(())
}

#[test]
fn per_step_type_sets_match_official_handlers() -> TestResult<()> {
  let msg = compat_job(
    Some(("actions_uses_cache_service_v2", "true")),
    Some("write"),
  )?;
  assert_eq!(
    msg
      .variables
      .get("actions_uses_cache_service_v2")
      .map(|v| v.value.as_str()),
    Some("true"),
    "raw job message must carry actions_uses_cache_service_v2 before extraction"
  );
  let env = forward_env(&extract_service_urls(&msg));
  let node = map(env.node);
  let container = map(env.container);
  let script = map(env.script);

  for key in [
    "ACTIONS_RESULTS_URL",
    "ACTIONS_RUNTIME_URL",
    "ACTIONS_CACHE_URL",
    "ACTIONS_RUNTIME_TOKEN",
    "ACTIONS_ID_TOKEN_REQUEST_URL",
    "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
    "ACTIONS_CACHE_MODE",
  ] {
    assert!(node.contains_key(key), "node missing {key}");
    assert!(container.contains_key(key), "container missing {key}");
  }
  assert_eq!(
    node.get("ACTIONS_CACHE_SERVICE_V2").map(String::as_str),
    Some("True")
  );
  assert!(!container.contains_key("ACTIONS_CACHE_SERVICE_V2"));
  assert_eq!(
    node.get("ACTIONS_CACHE_MODE").map(String::as_str),
    Some("write")
  );
  let mut script_keys: Vec<_> = script.keys().map(String::as_str).collect();
  script_keys.sort_unstable();
  assert_eq!(
    script_keys,
    [
      "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
      "ACTIONS_ID_TOKEN_REQUEST_URL"
    ],
    "run: steps get only the OIDC pair"
  );
  Ok(())
}

#[test]
fn cache_v2_flag_parses_as_boolean_case_insensitively() -> TestResult<()> {
  let cases = [
    (Some(("actions_uses_cache_service_v2", "true")), true),
    (Some(("actions_uses_cache_service_v2", "True")), true),
    (Some(("ACTIONS_USES_CACHE_SERVICE_V2", "TRUE")), true),
    (Some(("Actions_Uses_Cache_Service_V2", "true")), true),
    (Some(("actions_uses_cache_service_v2", "false")), false),
    (Some(("actions_uses_cache_service_v2", "1")), false),
    (Some(("actions_uses_cache_service_v2", "")), false),
    (Some(("ACTIONS_CACHE_SERVICE_V2", "true")), false),
    (None, false),
  ];
  for (flag, expected) in cases {
    for mode in [None, Some(""), Some("write")] {
      let env = forward_env(&extract_service_urls(&compat_job(flag, mode)?));
      let node = map(env.node);
      let container = map(env.container);
      let script = map(env.script);
      assert_eq!(
        node.get("ACTIONS_CACHE_SERVICE_V2").map(String::as_str),
        expected.then_some("True"),
        "flag {flag:?}"
      );
      // The mode is independent of the flag and exported unchanged.
      assert_eq!(
        node.get("ACTIONS_CACHE_MODE").map(String::as_str),
        mode.filter(|m| !m.is_empty()),
        "mode {mode:?} with flag {flag:?}"
      );
      // Container gets the mode but never the v2 flag; `run:` gets neither.
      assert_eq!(
        container
          .get("ACTIONS_CACHE_SERVICE_V2")
          .map(String::as_str),
        None,
        "container flag {flag:?}"
      );
      assert_eq!(
        container.get("ACTIONS_CACHE_MODE").map(String::as_str),
        mode.filter(|m| !m.is_empty()),
        "container mode {mode:?} with flag {flag:?}"
      );
      assert_eq!(
        script.get("ACTIONS_CACHE_SERVICE_V2").map(String::as_str),
        None,
        "script flag {flag:?}"
      );
      assert_eq!(
        script.get("ACTIONS_CACHE_MODE").map(String::as_str),
        None,
        "script mode {mode:?} with flag {flag:?}"
      );
    }
  }
  Ok(())
}

#[test]
fn forward_env_omits_none_fields_never_empty() {
  let mut urls = full_urls();
  urls.cache_url = None;
  urls.id_token_request_url = None;

  let env = forward_env(&urls);
  let node = map(env.node);

  assert!(node.contains_key("ACTIONS_RESULTS_URL"));
  assert!(node.contains_key("ACTIONS_RUNTIME_URL"));
  assert!(!node.contains_key("ACTIONS_CACHE_URL"));
  assert!(!node.contains_key("ACTIONS_ID_TOKEN_REQUEST_URL"));
  // The OIDC token is only set together with its URL.
  assert!(!node.contains_key("ACTIONS_ID_TOKEN_REQUEST_TOKEN"));
  assert!(env.script.is_empty());
  assert!(node.values().all(|v| !v.is_empty()));
}

#[test]
fn missing_runtime_token_omits_every_service_var() {
  let mut urls = full_urls();
  urls.runtime_token = String::new();
  let env = forward_env(&urls);
  assert!(env.node.is_empty());
  assert!(env.container.is_empty());
  assert!(env.script.is_empty());
}

/// Run a single echo step through the full `run_job` path and return the
/// concatenated stdout (`Log`) lines.
async fn run_echo_job(body: &str, mode: ServicesMode) -> TestResult<String> {
  Ok(run_echo_job_with_masker(body, mode).await?.0)
}

/// Drain the engine event stream into the concatenated stdout `Log` lines.
fn spawn_stdout_collector(mut rx: mpsc::Receiver<RunnerEvent>) -> tokio::task::JoinHandle<String> {
  tokio::spawn(async move {
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
  })
}

/// Same as `run_echo_job` but also returns the job's `SecretMasker`, so tests
/// can assert what the run registered for redaction.
async fn run_echo_job_with_masker(
  body: &str,
  mode: ServicesMode,
) -> TestResult<(String, Arc<Mutex<SecretMasker>>)> {
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
    ..RunnerConfig::default()
  };

  let mut msg = fixture_job()?;
  msg.steps = vec![ActionStep::script("echo-env", body, "")];

  let masker = Arc::new(Mutex::new(SecretMasker::new()));
  let (tx, rx) = mpsc::channel::<RunnerEvent>(1024);
  let collector = spawn_stdout_collector(rx);

  let teardown = run_job(
    msg,
    &config,
    CancellationToken::new(),
    tx,
    Arc::clone(&masker),
  )
  .await?;
  let lines = collector.await?;
  // `run_job` now defers cache maintenance + workspace GC to the caller.
  teardown.finish(&config).await;
  Ok((lines, masker))
}

#[tokio::test]
async fn runtime_token_registered_in_masker() -> TestResult<()> {
  // AC-2 (local half): setup_job_env must register the forwarded runtime
  // token with the SecretMasker so no log/journal sink sees it unredacted.
  let (_, masker) = run_echo_job_with_masker("true", ServicesMode::Forwarder).await?;
  let line = format!("token is {FIX_TOKEN} here");
  let redacted = masker
    .lock()
    .map_err(|e| format!("masker lock poisoned: {e}"))?
    .mask(&line);
  assert!(
    !redacted.contains(FIX_TOKEN),
    "runtime token was not registered with the masker; mask() output: {redacted}"
  );
  Ok(())
}

#[tokio::test]
async fn run_step_sees_only_the_oidc_pair() -> TestResult<()> {
  // Real assembly path (run_job → setup_job_env → step env): a `run:` step
  // gets the OIDC pair and no longer receives `ACTIONS_RUNTIME_TOKEN` or the service URLs.
  let out = run_echo_job(
    "printf 'OIDC=%s\\nRESULTS=%s\\nRUNTIME=%s\\nCACHE=%s\\n' \"$ACTIONS_ID_TOKEN_REQUEST_URL\" \"$ACTIONS_RESULTS_URL\" \"$ACTIONS_RUNTIME_TOKEN\" \"$ACTIONS_CACHE_URL\"",
    ServicesMode::Forwarder,
  )
  .await?;
  assert!(out.contains("OIDC=https://"), "got:\n{out}");
  assert!(out.contains("RESULTS=\n"), "got:\n{out}");
  assert!(out.contains("RUNTIME=\n"), "got:\n{out}");
  assert!(out.contains("CACHE=\n"), "got:\n{out}");
  Ok(())
}

#[tokio::test]
async fn offline_run_step_does_not_see_local_cache_vars() -> TestResult<()> {
  let out = run_echo_job(
    "printf 'CACHE=%s\\nTOKEN=%s\\n' \"$ACTIONS_CACHE_URL\" \"$ACTIONS_RUNTIME_TOKEN\"",
    ServicesMode::Offline,
  )
  .await?;
  assert!(
    out.contains("CACHE=\n") && out.contains("TOKEN=\n"),
    "got:\n{out}"
  );
  Ok(())
}

/// Run the probe Node action through the real step loop with `env` installed
/// and return the `ACTIONS_*` variables it printed.
async fn probe_node_action(env: ServiceEnv) -> TestResult<EnvMap> {
  use crate::gh_compat_node_action_file_commands::{seed_node, system_node};
  use execution::execution::actions::prefetch::ActionFetcher;
  use execution::execution::context::ExecutionContext;
  use execution::execution::steps_runner::{JobRun, run_steps};
  let node = system_node().ok_or("this test needs a real `node` runtime on PATH")?;
  let dir = tempfile::tempdir()?;
  let (workspace, data_dir) = (dir.path().join("work"), dir.path().join("data"));
  let action_dir = workspace.join("service-env-probe");
  std::fs::create_dir_all(&action_dir)?;
  std::fs::create_dir_all(&data_dir)?;
  for name in ["action.yml", "main.js"] {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
      .join("tests/fixtures/service_env_action")
      .join(name);
    // Unfilled markers would make the probe write a stray file and wait for a
    // sentinel, so blank them (no dump file, no hold).
    let body = std::fs::read_to_string(src)?
      .replace("__DUMP__", "")
      .replace("__SENTINEL__", "");
    std::fs::write(action_dir.join(name), body)?;
  }
  seed_node(&data_dir, &node)?;
  let config = RunnerConfig {
    data_dir,
    workspace_root: workspace.clone(),
    cgroup_path: None,
    ..RunnerConfig::default()
  };
  let mut ctx = ExecutionContext::with_masker(Arc::new(Mutex::new(SecretMasker::new())));
  ctx.set_service_env(env);
  let (tx, mut rx) = mpsc::channel::<RunnerEvent>(1024);
  let collector = tokio::spawn(async move {
    let mut lines = Vec::new();
    while let Some(event) = rx.recv().await {
      if let RunnerEvent::Log { line, .. } = event {
        lines.push(line);
      }
    }
    lines
  });
  let mut step = ActionStep::with_ref_type("probe", "repository");
  step.reference.name = Some("./service-env-probe".to_owned());
  let spec = execution::execution::job_spec::JobSpec::default();
  let http = shared_client().clone();
  let fetcher = ActionFetcher::new(&shared::NetworkConfig::default())?;
  let conclusion = run_steps(
    &[step],
    &mut ctx,
    &tx,
    CancellationToken::new(),
    &JobRun {
      workspace: &workspace,
      config: &config,
      spec: &spec,
      shadow: None,
      http: &http,
      fetcher: &fetcher,
    },
  )
  .await?;
  drop(tx);
  let lines = collector.await?;
  assert_eq!(conclusion, shared::Conclusion::Success, "log: {lines:?}");
  let json = lines
    .iter()
    .find_map(|line| line.strip_prefix("PROBE "))
    .ok_or("probe line missing")?;
  Ok(serde_json::from_str(json)?)
}

#[tokio::test]
async fn node_action_receives_the_forwarded_set_with_v2_flag_and_mode() -> TestResult<()> {
  let msg = compat_job(
    Some(("actions_uses_cache_service_v2", "true")),
    Some("write"),
  )?;
  let env = forward_env(&extract_service_urls(&msg));
  let expected = map(env.node.clone());
  let seen = tokio::time::timeout(std::time::Duration::from_secs(300), probe_node_action(env))
    .await
    .map_err(|_| "probe_node_action timed out after 5 minutes")??;
  // The host process (e.g. a CI job) may carry its own ACTIONS_* vars, so check
  // every forwarded pair arrived with its exact value rather than map equality.
  for (key, value) in &expected {
    assert_eq!(seen.get(key), Some(value), "{key}");
  }
  assert_eq!(
    seen.get("ACTIONS_CACHE_SERVICE_V2").map(String::as_str),
    Some("True")
  );
  assert_eq!(
    seen.get("ACTIONS_CACHE_MODE").map(String::as_str),
    Some("write")
  );
  Ok(())
}

/// Install the cache-client action twice (`save` / `restore`) in `workspace`
/// and return its two `uses:` steps.
fn cache_client_steps(
  workspace: &std::path::Path,
  file: &std::path::Path,
) -> TestResult<Vec<ActionStep>> {
  let fixture =
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cache_client_action");
  let mut steps = Vec::new();
  for mode in ["save", "restore"] {
    let dir = workspace.join(format!("cache-{mode}"));
    std::fs::create_dir_all(&dir)?;
    std::fs::copy(fixture.join("action.yml"), dir.join("action.yml"))?;
    let main = std::fs::read_to_string(fixture.join("main.js"))?
      .replace("__MODE__", mode)
      .replace("__FILE__", &file.to_string_lossy());
    std::fs::write(dir.join("main.js"), main)?;
    let mut step = ActionStep::with_ref_type(mode, "repository");
    step.reference.name = Some(format!("./cache-{mode}"));
    steps.push(step);
  }
  Ok(steps)
}

#[tokio::test]
async fn node_action_saves_and_restores_exact_bytes_over_v2_with_no_v1_request() -> TestResult<()> {
  tokio::time::timeout(
    std::time::Duration::from_secs(300),
    node_action_saves_and_restores_inner(),
  )
  .await
  .map_err(|_| "cache v2 save/restore timed out after 5 minutes")??;
  Ok(())
}

async fn node_action_saves_and_restores_inner() -> TestResult<()> {
  use cache::blob::BlobRegistry;
  use cache::cas::{CacheIndex, CasStore, LeaseSet};
  use cache::scope::CacheScopes;
  use cache::trust::TrustLevel;
  use cache::twirp::{TwirpState, cache_router};
  use execution::execution::actions::prefetch::ActionFetcher;
  use execution::execution::context::ExecutionContext;
  use execution::execution::steps_runner::{JobRun, run_steps};

  let node = crate::gh_compat_node_action_file_commands::system_node()
    .ok_or("this test needs a real `node` runtime on PATH")?;
  let dir = tempfile::tempdir()?;
  let (workspace, data_dir) = (dir.path().join("work"), dir.path().join("data"));
  let cas_root = dir.path().join("cas");
  std::fs::create_dir_all(&workspace)?;
  std::fs::create_dir_all(&data_dir)?;
  std::fs::create_dir_all(cas_root.join("staging"))?;
  crate::gh_compat_node_action_file_commands::seed_node(&data_dir, &node)?;

  // Loopback v2 peer: the real cache router, recording every request path.
  let bearer = "peer-token".to_owned();
  let state = TwirpState {
    store: CasStore::new(cas_root.clone(), 16384, 1 << 30),
    index: CacheIndex::new(cas_root.clone()),
    registry: BlobRegistry::new(),
    leases: LeaseSet::new(),
    scopes: CacheScopes {
      write: "refs/heads/main".to_owned(),
      read_ladder: vec!["refs/heads/main".to_owned()],
    },
    trust: TrustLevel::Trusted,
    protected: Vec::new(),
    bearer: bearer.clone(),
    staging_root: cas_root.join("staging"),
    upload_ttl: std::time::Duration::from_secs(300),
    download_ttl: std::time::Duration::from_secs(300),
  };
  let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
  let log = Arc::clone(&seen);
  let app = cache_router(state).layer(axum::middleware::from_fn(
    move |req: axum::extract::Request, next: axum::middleware::Next| {
      let log = Arc::clone(&log);
      async move {
        if let Ok(mut paths) = log.lock() {
          paths.push(req.uri().path().to_owned());
        }
        next.run(req).await
      }
    },
  ));
  let server = cache::server::CacheServer::start(app, "127.0.0.1:0").await?;

  let msg = compat_job(Some(("actions_uses_cache_service_v2", "true")), None)?;
  let mut env = forward_env(&extract_service_urls(&msg));
  env.set_action("ACTIONS_RESULTS_URL", server.base_url());
  env.set_action("ACTIONS_CACHE_URL", server.base_url());
  env.set_action("ACTIONS_RUNTIME_TOKEN", &bearer);

  let payload =
    std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock"))?;
  let file = dir.path().join("payload.bin");
  std::fs::write(&file, &payload)?;
  let mut steps = cache_client_steps(&workspace, &file)?.into_iter();
  let (Some(save_step), Some(restore_step)) = (steps.next(), steps.next()) else {
    return Err("save and restore steps".into());
  };

  let mut ctx = ExecutionContext::with_masker(Arc::new(Mutex::new(SecretMasker::new())));
  ctx.set_service_env(env);
  let (tx, mut rx) = mpsc::channel::<RunnerEvent>(1024);
  let collector = tokio::spawn(async move {
    let mut lines = Vec::new();
    while let Some(ev) = rx.recv().await {
      if let RunnerEvent::Log { line, .. } = ev {
        lines.push(line);
      }
    }
    lines
  });
  let config = RunnerConfig {
    data_dir,
    workspace_root: workspace.clone(),
    cgroup_path: None,
    ..RunnerConfig::default()
  };
  let spec = execution::execution::job_spec::JobSpec::default();
  let http = shared_client().clone();
  let fetcher = ActionFetcher::new(&shared::NetworkConfig::default())?;
  // Save, wipe the file, then restore — separate Node processes.
  let save = run_steps(
    std::slice::from_ref(&save_step),
    &mut ctx,
    &tx,
    CancellationToken::new(),
    &JobRun {
      workspace: &workspace,
      config: &config,
      spec: &spec,
      shadow: None,
      http: &http,
      fetcher: &fetcher,
    },
  )
  .await?;
  std::fs::remove_file(&file)?;
  let restore = run_steps(
    std::slice::from_ref(&restore_step),
    &mut ctx,
    &tx,
    CancellationToken::new(),
    &JobRun {
      workspace: &workspace,
      config: &config,
      spec: &spec,
      shadow: None,
      http: &http,
      fetcher: &fetcher,
    },
  )
  .await?;
  drop(tx);
  let lines = collector.await?;
  assert_eq!(save, shared::Conclusion::Success, "save log: {lines:?}");
  assert_eq!(
    restore,
    shared::Conclusion::Success,
    "restore log: {lines:?}"
  );
  assert_eq!(std::fs::read(&file)?, payload, "restored bytes differ");

  let paths = seen.lock().map_err(|e| format!("poisoned: {e}"))?.clone();
  assert!(
    paths.iter().any(|p| p.ends_with("/CreateCacheEntry")),
    "{paths:?}"
  );
  assert!(
    paths
      .iter()
      .any(|p| p.ends_with("/GetCacheEntryDownloadURL")),
    "{paths:?}"
  );
  assert!(
    !paths.iter().any(|p| p.contains("_apis/artifactcache")),
    "no v1 request allowed: {paths:?}"
  );
  Ok(())
}

#[tokio::test]
async fn composite_run_step_gets_only_the_oidc_pair() -> TestResult<()> {
  use execution::execution::actions::prefetch::ActionFetcher;
  use execution::execution::context::ExecutionContext;
  use execution::execution::steps_runner::{JobRun, run_steps};

  let dir = tempfile::tempdir()?;
  let (workspace, data_dir) = (dir.path().join("work"), dir.path().join("data"));
  let action_dir = workspace.join("comp");
  std::fs::create_dir_all(&action_dir)?;
  std::fs::create_dir_all(&data_dir)?;
  std::fs::write(
    action_dir.join("action.yml"),
    "name: comp\ndescription: probe\nruns:\n  using: composite\n  steps:\n    - shell: bash\n      env:\n        ACTIONS_ID_TOKEN_REQUEST_URL: spoofed\n      run: echo \"COMP oidc=$ACTIONS_ID_TOKEN_REQUEST_URL token=[$ACTIONS_RUNTIME_TOKEN] results=[$ACTIONS_RESULTS_URL]\"\n",
  )?;
  let msg = compat_job(None, None)?;
  let mut ctx = ExecutionContext::with_masker(Arc::new(Mutex::new(SecretMasker::new())));
  ctx.set_service_env(forward_env(&extract_service_urls(&msg)));
  let (tx, mut rx) = mpsc::channel::<RunnerEvent>(1024);
  let collector = tokio::spawn(async move {
    let mut lines = Vec::new();
    while let Some(ev) = rx.recv().await {
      if let RunnerEvent::Log { line, .. } = ev {
        lines.push(line);
      }
    }
    lines
  });
  let mut step = ActionStep::with_ref_type("comp", "repository");
  step.reference.name = Some("./comp".to_owned());
  let config = RunnerConfig {
    data_dir,
    workspace_root: workspace.clone(),
    cgroup_path: None,
    ..RunnerConfig::default()
  };
  let spec = execution::execution::job_spec::JobSpec::default();
  let http = shared_client().clone();
  let fetcher = ActionFetcher::new(&shared::NetworkConfig::default())?;
  let conclusion = run_steps(
    &[step],
    &mut ctx,
    &tx,
    CancellationToken::new(),
    &JobRun {
      workspace: &workspace,
      config: &config,
      spec: &spec,
      shadow: None,
      http: &http,
      fetcher: &fetcher,
    },
  )
  .await?;
  drop(tx);
  let lines = collector.await?;
  assert_eq!(conclusion, shared::Conclusion::Success, "log: {lines:?}");
  let line = lines
    .iter()
    .find(|l| l.starts_with("COMP "))
    .ok_or("composite line missing")?;
  assert!(line.contains("oidc=https://"), "{line}");
  assert!(
    line.contains("token=[]") && line.contains("results=[]"),
    "{line}"
  );
  Ok(())
}
