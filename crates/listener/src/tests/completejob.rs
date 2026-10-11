//! Issue 88: the `completejob` body built by the production lifecycle
//! (`run_acquired_job` → `report_completion` → `wire::net::complete_job`) for
//! sanitized github.com acquisitions from `completejob-88.yml` run
//! 38098611227, executed by the real engine with real `bash`/`node`.
//!
//! The Results Service and live-log endpoints are removed from the captures,
//! so nothing here reaches GitHub; the Run Service is a local recording
//! server. Fault-injection servers (labelled below) stand in for GitHub's
//! action resolver; their injected statuses are the faults under test.

use std::collections::HashMap;
use std::error::Error;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use shared::SecretMasker;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use wire::reporting::run_service::AcquireJobResponse;

use super::{SessionCtx, report_completion, run_acquired_job};
use crate::support::RecordingServer;

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

const ENV: &str = include_str!("../../../execution/tests/completejob_88_env_message.json");
const SECRET: &str = include_str!("../../../execution/tests/completejob_88_secret_message.json");
const STEPS: &str = include_str!("../../../execution/tests/completejob_88_steps_message.json");
const CAPTURED_URL: &str = "https://toolu-88.example/toolu/38098611227";
const ACTIONS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../.github/actions");
/// Every key upstream's `StepResult.cs` can serialize (that toolu sends).
const STEP_KEYS: [&str; 13] = [
  "external_id",
  "number",
  "name",
  "action_name",
  "ref",
  "type",
  "status",
  "conclusion",
  "started_at",
  "completed_at",
  "completed_log_url",
  "completed_log_lines",
  "annotations",
];

/// A capture with its Results Service and live-log endpoints removed.
fn offline(capture: &str) -> TestResult<Value> {
  let mut job: Value = serde_json::from_str(capture)?;
  if let Some(variables) = job.get_mut("variables").and_then(Value::as_object_mut) {
    variables.remove("system.github.results_endpoint");
  }
  for endpoint in job
    .pointer_mut("/resources/endpoints")
    .and_then(Value::as_array_mut)
    .into_iter()
    .flatten()
  {
    if let Some(data) = endpoint.get_mut("data").and_then(Value::as_object_mut) {
      data.remove("FeedStreamUrl");
    }
  }
  Ok(job)
}

fn set(job: &mut Value, pointer: &str, value: Value) -> TestResult {
  *job
    .pointer_mut(pointer)
    .ok_or_else(|| format!("{pointer} missing"))? = value;
  Ok(())
}

fn set_script(job: &mut Value, index: usize, script: &str) -> TestResult {
  let entries = job
    .pointer_mut(&format!("/steps/{index}/inputs/map"))
    .and_then(Value::as_array_mut)
    .ok_or("captured inputs missing")?;
  let entry = entries
    .iter_mut()
    .find(|entry| entry.pointer("/Key/lit").and_then(Value::as_str) == Some("script"))
    .ok_or("captured script missing")?;
  if let Some(object) = entry.as_object_mut() {
    object.insert("Value".to_owned(), json!({"type": 0, "lit": script}));
  }
  Ok(())
}

struct Harness {
  ctx: SessionCtx,
  _dir: tempfile::TempDir,
  journal: mpsc::Receiver<shared::ListenerEvent>,
}

fn harness() -> TestResult<Harness> {
  let dir = tempfile::tempdir()?;
  let config = shared::RunnerConfig {
    data_dir: dir.path().join("data"),
    workspace_root: dir.path().join("work"),
    workspace_gc_hours: 0,
    ..shared::RunnerConfig::default()
  };
  let (tx, journal) = mpsc::channel(4096);
  let ctx = SessionCtx {
    runner_name: Some("toolu-88-test".to_owned()),
    client: reqwest::Client::new(),
    token: String::new(),
    broker_url: String::new(),
    session_id: String::new(),
    config,
    masker: Arc::new(Mutex::new(SecretMasker::new())),
    cancel: CancellationToken::new(),
    tx,
    encryption_key: None,
    use_fips_encryption: false,
    rsa_private_key_der: Vec::new(),
    refresh_auth: None,
    live_log: None,
    job_log_upload: None,
    watchdog: crate::helpers::WatchdogConfig::default(),
  };
  Ok(Harness {
    ctx,
    _dir: dir,
    journal,
  })
}

/// Run the acquired job and report it; returns every `completejob` body.
async fn complete(
  harness: &Harness,
  job: Value,
  run_service: &str,
  cancel: &CancellationToken,
) -> TestResult<()> {
  let plan_id = job
    .pointer("/plan/planId")
    .and_then(Value::as_str)
    .ok_or("captured plan id missing")?
    .to_owned();
  let acquired = AcquireJobResponse {
    plan_id: plan_id.clone(),
    body: job,
    run_service_token: None,
  };
  let outcome = tokio::time::timeout(
    std::time::Duration::from_secs(120),
    run_acquired_job(&harness.ctx, run_service, "run-token", &acquired, cancel),
  )
  .await?;
  report_completion(
    &harness.ctx,
    run_service,
    plan_id,
    "run-token".to_owned(),
    outcome,
  )
  .await?;
  Ok(())
}

async fn completed_body(harness: &Harness, job: Value) -> TestResult<Value> {
  let server = RecordingServer::start(|_| HashMap::new()).await;
  complete(harness, job, &server.base_url(), &CancellationToken::new()).await?;
  let request = server
    .requests()
    .into_iter()
    .find(|request| request.path.ends_with("completejob"))
    .ok_or("completejob was not posted")?;
  Ok(serde_json::from_slice(&request.body)?)
}

fn step<'a>(body: &'a Value, name: &str) -> TestResult<&'a Value> {
  body
    .get("stepResults")
    .and_then(Value::as_array)
    .and_then(|steps| {
      steps
        .iter()
        .find(|step| step.get("name") == Some(&json!(name)))
    })
    .ok_or_else(|| format!("step {name:?} missing from {body}").into())
}

fn identity(step: &Value) -> (Option<&str>, Option<&str>, Option<&str>) {
  (
    step.get("type").and_then(Value::as_str),
    step.get("action_name").and_then(Value::as_str),
    step.get("ref").and_then(Value::as_str),
  )
}

fn assert_upstream_keys(body: &Value) -> TestResult {
  for step in body
    .get("stepResults")
    .and_then(Value::as_array)
    .ok_or("stepResults missing")?
  {
    for key in step.as_object().ok_or("step not an object")?.keys() {
      assert!(
        STEP_KEYS.contains(&key.as_str()),
        "non-upstream key {key} in {step}"
      );
    }
    assert_eq!(step.get("status"), Some(&json!("completed")));
    assert!(step.get("annotations").is_some_and(Value::is_array));
  }
  Ok(())
}

#[tokio::test]
async fn completejob_env_capture_sends_url_billing_and_step_identity() -> TestResult {
  let harness = harness()?;
  let job = offline(ENV)?;
  let deploy_id = job
    .pointer("/steps/0/id")
    .and_then(Value::as_str)
    .ok_or("id")?
    .to_owned();
  let body = completed_body(&harness, job).await?;
  assert_eq!(body.get("environmentUrl"), Some(&json!(CAPTURED_URL)));
  assert_eq!(body.get("billingOwnerId"), Some(&json!("U_kgDOAH-IyQ")));
  assert_eq!(body.get("infrastructureFailureCategory"), None);
  assert_eq!(body.get("conclusion"), Some(&json!("succeeded")));
  assert_upstream_keys(&body)?;
  let setup = step(&body, "Set up job")?;
  assert_eq!(identity(setup), (Some("runner"), Some("setup_job"), None));
  let deploy = step(&body, "Deploy")?;
  // The wire UUID, never the expression context name `deploy`.
  assert_eq!(deploy.get("external_id"), Some(&json!(deploy_id)));
  assert_eq!(identity(deploy), (Some("run"), Some("bash"), None));
  let complete = step(&body, "Complete job")?;
  assert_eq!(
    identity(complete),
    (Some("runner"), Some("complete_job"), None)
  );
  assert_eq!(complete.get("annotations"), Some(&json!([])));
  let numbers: Vec<u64> = body
    .get("stepResults")
    .and_then(Value::as_array)
    .ok_or("stepResults")?
    .iter()
    .filter_map(|step| step.get("number").and_then(Value::as_u64))
    .collect();
  assert_eq!(
    complete.get("number").and_then(Value::as_u64),
    numbers.iter().copied().max()
  );
  Ok(())
}

#[tokio::test]
async fn completejob_without_billing_owner_omits_it() -> TestResult {
  let harness = harness()?;
  let mut job = offline(ENV)?;
  job.as_object_mut().ok_or("job")?.remove("billingOwnerId");
  let body = completed_body(&harness, job).await?;
  assert_eq!(body.get("billingOwnerId"), None);
  assert_eq!(body.get("environmentUrl"), Some(&json!(CAPTURED_URL)));
  Ok(())
}

#[tokio::test]
async fn secret_environment_url_never_reaches_completejob_or_the_masker_sinks() -> TestResult {
  const TOKEN: &str = "disposable-88-fixed-token";
  let mut harness = harness()?;
  let mut job = offline(SECRET)?;
  // Capture-derived: the captured step's random token becomes a fixed one.
  set_script(
    &mut job,
    0,
    &format!(
      "echo \"::add-mask::{TOKEN}\"\necho \"url=https://toolu-88.example/{TOKEN}\" >> \"$GITHUB_OUTPUT\"\necho \"deploying {TOKEN}\""
    ),
  )?;
  let name = job
    .pointer("/actionsEnvironment/name")
    .and_then(Value::as_str)
    .ok_or("name")?
    .to_owned();
  let body = completed_body(&harness, job).await?;
  assert_eq!(body.get("environmentUrl"), None);
  assert!(
    !body.to_string().contains(TOKEN),
    "secret in completejob: {body}"
  );
  let warning = step(&body, "Complete job")?
    .get("annotations")
    .and_then(Value::as_array)
    .and_then(|annotations| annotations.first())
    .cloned()
    .ok_or("Complete job warning missing")?;
  assert_eq!(warning.get("level"), Some(&json!(2)));
  assert_eq!(
    warning.get("message"),
    Some(&json!(format!(
      "Skip setting environment url as environment '{name}' may contain secret."
    )))
  );
  // Logs and the journal mask through the session masker: the runtime
  // `::add-mask::` reached it, so no masked line can carry the token.
  let masker = harness.ctx.masker.lock().map_err(|e| e.to_string())?;
  let mut saw_token = false;
  while let Ok(event) = harness.journal.try_recv() {
    if let shared::ListenerEvent::Runner(shared::RunnerEvent::Log { line, .. }) = event {
      saw_token |= line.contains(TOKEN);
      assert!(!masker.mask(&line).contains(TOKEN), "unmasked: {line}");
    }
  }
  assert!(saw_token, "the probe must actually log the token");
  Ok(())
}

#[tokio::test]
async fn mask_hint_url_is_suppressed() -> TestResult {
  let harness = harness()?;
  let mut job = offline(ENV)?;
  job
    .get_mut("mask")
    .and_then(Value::as_array_mut)
    .ok_or("mask")?
    .push(json!({"type": "regex", "value": "toolu-88.example"}));
  let body = completed_body(&harness, job).await?;
  assert_eq!(body.get("environmentUrl"), None);
  assert_eq!(
    step(&body, "Complete job")?
      .get("annotations")
      .and_then(Value::as_array)
      .map(Vec::len),
    Some(1)
  );
  Ok(())
}

#[tokio::test]
async fn completejob_retry_resends_an_identical_body() -> TestResult {
  // Run Service stand-in: the first completejob gets a 503, the rest 200.
  let bodies = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
  let attempts = Arc::new(AtomicUsize::new(0));
  let (record, count) = (Arc::clone(&bodies), Arc::clone(&attempts));
  let app = axum::Router::new().fallback(move |request: axum::extract::Request| {
    let (record, count) = (Arc::clone(&record), Arc::clone(&count));
    async move {
      let completes = request.uri().path().ends_with("completejob");
      let body = axum::body::to_bytes(request.into_body(), usize::MAX)
        .await
        .map(|bytes| bytes.to_vec())
        .unwrap_or_default();
      if !completes {
        return axum::http::StatusCode::OK;
      }
      if let Ok(mut bodies) = record.lock() {
        bodies.push(body);
      }
      if count.fetch_add(1, Ordering::SeqCst) == 0 {
        axum::http::StatusCode::SERVICE_UNAVAILABLE
      } else {
        axum::http::StatusCode::OK
      }
    }
  });
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
  let base = format!("http://{}", listener.local_addr()?);
  tokio::spawn(async move {
    if let Err(error) = axum::serve(listener, app).await {
      eprintln!("run service stand-in stopped: {error}");
    }
  });
  let harness = harness()?;
  complete(&harness, offline(ENV)?, &base, &CancellationToken::new()).await?;
  let bodies = bodies.lock().map_err(|e| e.to_string())?.clone();
  let [first, second] = bodies.as_slice() else {
    return Err(format!("expected two completejob posts, got {}", bodies.len()).into());
  };
  assert_eq!(first, second);
  let body: Value = serde_json::from_slice(second)?;
  assert_eq!(body.get("environmentUrl"), Some(&json!(CAPTURED_URL)));
  assert_eq!(body.get("billingOwnerId"), Some(&json!("U_kgDOAH-IyQ")));
  assert_eq!(
    identity(step(&body, "Deploy")?),
    (Some("run"), Some("bash"), None)
  );
  Ok(())
}

#[tokio::test]
async fn completejob_cancelled_job_keeps_the_url() -> TestResult {
  let harness = harness()?;
  let mut job = offline(ENV)?;
  set_script(&mut job, 1, "sleep 60")?;
  let server = RecordingServer::start(|_| HashMap::new()).await;
  let cancel = CancellationToken::new();
  let trigger = cancel.clone();
  tokio::spawn(async move {
    tokio::time::sleep(std::time::Duration::from_secs(4)).await;
    trigger.cancel();
  });
  complete(&harness, job, &server.base_url(), &cancel).await?;
  let request = server
    .requests()
    .into_iter()
    .find(|request| request.path.ends_with("completejob"))
    .ok_or("completejob was not posted")?;
  let body: Value = serde_json::from_slice(&request.body)?;
  assert_eq!(body.get("conclusion"), Some(&json!("canceled")));
  assert_eq!(body.get("environmentUrl"), Some(&json!(CAPTURED_URL)));
  Ok(())
}

async fn seed_node(data: &std::path::Path) -> TestResult {
  let output = tokio::process::Command::new("node")
    .args(["-e", "process.stdout.write(process.execPath)"])
    .output()
    .await?;
  assert!(output.status.success(), "Node is required for this test");
  let target = execution::node::runtime::node_binary_path(
    &execution::node::runtime::node_cache_dir(data, execution::node::runtime::node_version_for(20)),
  );
  std::fs::create_dir_all(target.parent().ok_or("node cache parent")?)?;
  #[cfg(unix)]
  std::os::unix::fs::symlink(String::from_utf8(output.stdout)?.trim(), target)?;
  Ok(())
}

#[tokio::test]
async fn step_metadata_reaches_every_posted_step_result() -> TestResult {
  let harness = harness()?;
  let mut job = offline(STEPS)?;
  // bash, sh, local node with post, local composite, failing continue-on-error.
  let steps = job
    .get("steps")
    .and_then(Value::as_array)
    .cloned()
    .ok_or("steps")?;
  let kept: Vec<Value> = [1, 2, 3, 4, 7]
    .iter()
    .filter_map(|index| steps.get(*index).cloned())
    .collect();
  set(&mut job, "/steps", Value::Array(kept))?;
  let job_id = job.get("jobId").and_then(Value::as_str).ok_or("jobId")?;
  let workspace = harness.ctx.config.workspace_root.join(job_id);
  for name in ["completejob-88-node", "completejob-88-composite"] {
    let target = workspace.join(".github/actions").join(name);
    std::fs::create_dir_all(&target)?;
    for entry in std::fs::read_dir(std::path::Path::new(ACTIONS).join(name))? {
      let entry = entry?;
      std::fs::copy(entry.path(), target.join(entry.file_name()))?;
    }
  }
  seed_node(&harness.ctx.config.data_dir).await?;
  let body = completed_body(&harness, job).await?;
  assert_upstream_keys(&body)?;
  let node = (
    Some("node20"),
    Some("./.github/actions/completejob-88-node"),
    None,
  );
  for (name, expected) in [
    ("Bash script", (Some("run"), Some("bash"), None)),
    ("Sh script", (Some("run"), Some("sh"), None)),
    ("Local node with post", node),
    ("Post Local node with post", node),
    (
      "Local composite",
      (
        Some("composite"),
        Some("./.github/actions/completejob-88-composite"),
        None,
      ),
    ),
    (
      "Failing step with an error annotation",
      (Some("run"), Some("bash"), None),
    ),
  ] {
    assert_eq!(identity(step(&body, name)?), expected, "{name}");
  }
  // The failing step's `::error::` stays on it; continue-on-error made it succeed.
  let failing = step(&body, "Failing step with an error annotation")?;
  assert_eq!(failing.get("conclusion"), Some(&json!("succeeded")));
  assert_eq!(
    failing.pointer("/annotations/0/message"),
    Some(&json!("step failed on purpose"))
  );
  // The composite child's notice belongs to the composite step.
  assert_eq!(
    step(&body, "Local composite")?.pointer("/annotations/0/level"),
    Some(&json!(1))
  );
  assert_eq!(body.get("infrastructureFailureCategory"), None);
  Ok(())
}

/// Fault-injection action resolver: every download-info request gets `status`.
async fn failing_resolver(status: u16) -> TestResult<String> {
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
  let base = format!("http://{}", listener.local_addr()?);
  let app = axum::Router::new().fallback(move || async move {
    axum::http::StatusCode::from_u16(status)
      .unwrap_or(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
  });
  tokio::spawn(async move {
    if let Err(error) = axum::serve(listener, app).await {
      eprintln!("resolver stand-in stopped: {error}");
    }
  });
  Ok(base)
}

fn checkout_job(launch: &str, continue_on_error: bool) -> TestResult<Value> {
  let mut job = offline(STEPS)?;
  let steps = job
    .get("steps")
    .and_then(Value::as_array)
    .cloned()
    .ok_or("steps")?;
  let mut checkout = steps.first().cloned().ok_or("checkout")?;
  if continue_on_error && let Some(object) = checkout.as_object_mut() {
    object.insert(
      "continueOnError".to_owned(),
      json!({"type": 5, "bool": true}),
    );
  }
  let bash = steps.get(1).cloned().ok_or("bash")?;
  set(&mut job, "/steps", Value::Array(vec![checkout, bash]))?;
  set(
    &mut job,
    "/variables/system.github.launch_endpoint/value",
    json!(launch),
  )?;
  Ok(job)
}

#[tokio::test]
async fn infrastructure_category_is_sent_only_for_a_failed_runner_side_fetch() -> TestResult {
  let launch = failing_resolver(500).await?;
  let body = completed_body(&harness()?, checkout_job(&launch, false)?).await?;
  assert_eq!(body.get("conclusion"), Some(&json!("failed")));
  assert_eq!(
    body.get("infrastructureFailureCategory"),
    Some(&json!("resolve_action"))
  );
  let checkout = step(&body, "Run actions/checkout@v4")?;
  assert_eq!(
    checkout.pointer("/annotations/0/isInfrastructureIssue"),
    Some(&json!(true))
  );
  assert_eq!(checkout.pointer("/annotations/0/level"), Some(&json!(3)));

  // continue-on-error: the step turns green, so the job claims no category.
  let body = completed_body(&harness()?, checkout_job(&launch, true)?).await?;
  assert_eq!(body.get("conclusion"), Some(&json!("succeeded")));
  assert_eq!(body.get("infrastructureFailureCategory"), None);
  assert_eq!(
    step(&body, "Run actions/checkout@v4")?.pointer("/annotations/0/isInfrastructureIssue"),
    Some(&json!(true))
  );

  // A user error (unresolvable action) and a failing user script: no category.
  let unresolvable = failing_resolver(422).await?;
  let body = completed_body(&harness()?, checkout_job(&unresolvable, false)?).await?;
  assert_eq!(body.get("conclusion"), Some(&json!("failed")));
  assert_eq!(body.get("infrastructureFailureCategory"), None);
  let mut script = offline(ENV)?;
  set_script(&mut script, 1, "exit 7")?;
  let body = completed_body(&harness()?, script).await?;
  assert_eq!(body.get("conclusion"), Some(&json!("failed")));
  assert_eq!(body.get("infrastructureFailureCategory"), None);
  Ok(())
}

#[tokio::test]
async fn infrastructure_error_message_is_masked_in_completejob_and_the_journal() -> TestResult {
  let launch = failing_resolver(500).await?;
  let mut harness = harness()?;
  let mut job = checkout_job(&launch, false)?;
  // A registered secret that the resolver's status text happens to contain.
  job
    .get_mut("mask")
    .and_then(Value::as_array_mut)
    .ok_or("mask")?
    .push(json!({"type": "regex", "value": "Internal Server Error"}));
  let body = completed_body(&harness, job).await?;
  let masked = "action download-info status 500 ***";
  let checkout = step(&body, "Run actions/checkout@v4")?;
  assert_eq!(
    checkout.pointer("/annotations/0/message"),
    Some(&json!(masked))
  );
  assert_eq!(
    checkout.pointer("/annotations/0/isInfrastructureIssue"),
    Some(&json!(true))
  );
  let mut journaled = Vec::new();
  while let Ok(event) = harness.journal.try_recv() {
    if let shared::ListenerEvent::Runner(shared::RunnerEvent::InfrastructureError {
      message, ..
    }) = event
    {
      journaled.push(message);
    }
  }
  assert_eq!(journaled, vec![masked.to_owned()]);
  Ok(())
}

#[tokio::test]
async fn unparseable_job_still_echoes_the_billing_owner() -> TestResult {
  let harness = harness()?;
  let mut job = offline(ENV)?;
  // Capture-derived: a body the runner cannot parse as a job message.
  set(&mut job, "/steps", json!("not a step list"))?;
  let body = completed_body(&harness, job).await?;
  assert_eq!(body.get("conclusion"), Some(&json!("failed")));
  assert_eq!(body.get("billingOwnerId"), Some(&json!("U_kgDOAH-IyQ")));
  assert_eq!(body.get("environmentUrl"), None);
  assert_eq!(body.get("infrastructureFailureCategory"), None);
  Ok(())
}

#[tokio::test]
async fn first_failed_step_category_wins_and_only_failures_latch() -> TestResult {
  use shared::{Conclusion, RunnerEvent};
  let started = |id: &str, step_number| RunnerEvent::StepStarted {
    step_id: id.to_owned(),
    step_name: id.to_owned(),
    step_number,
  };
  let infra = |id: &str, category: &str| RunnerEvent::InfrastructureError {
    step_id: id.to_owned(),
    category: category.to_owned(),
    message: format!("{category} fault"),
  };
  let completed = |id: &str, conclusion| RunnerEvent::StepCompleted {
    step_id: id.to_owned(),
    conclusion,
    outputs: HashMap::new(),
  };
  for (events, expected) in [
    // Upstream keeps the first category (`ExecutionContext.Complete`).
    (
      vec![
        started("a", 2),
        infra("a", "resolve_action"),
        completed("a", Conclusion::Failure),
        started("b", 3),
        infra("b", "error_download_action"),
        completed("b", Conclusion::Failure),
      ],
      Some("resolve_action"),
    ),
    // A cancelled step never latches, and does not block a later failure.
    (
      vec![
        started("a", 2),
        infra("a", "resolve_action"),
        completed("a", Conclusion::Cancelled),
      ],
      None,
    ),
    (
      vec![
        started("a", 2),
        infra("a", "resolve_action"),
        completed("a", Conclusion::Cancelled),
        started("b", 3),
        infra("b", "invalid_action_download"),
        completed("b", Conclusion::Failure),
      ],
      Some("invalid_action_download"),
    ),
  ] {
    let collector = crate::step_reporter::StepCollector::new();
    for event in &events {
      collector.record(event).await;
    }
    assert_eq!(
      collector.infrastructure_category().await.as_deref(),
      expected
    );
  }
  Ok(())
}
