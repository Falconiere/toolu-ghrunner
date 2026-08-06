//! End-to-end fixture driving the REAL `job_lifecycle::poll_and_execute`
//! against one wiremock `MockServer` standing in for both the broker
//! (`/message`, `/acknowledge`) and the Run Service (`/acquirejob`,
//! `/renewjob`, `/completejob`) — the job message's `run_service_url` field
//! points at the same server the broker calls use, matching the real
//! github.com JIT wire shape where both addresses come from the mint.
//! Covers AC-2 (trip → kill → report `failure`), AC-9 (trip +
//! report-on-reconnect), AC-10 (a persistent definitive renewal error must
//! never trip). Filtered by the s6 ledger check `test(/^watchdog_tests::/)`.
//!
//! Split out of `watchdog_tests.rs` into its own file (nested module —
//! declared there as `mod trip;`) once the combined `retry` + `trip` file
//! passed the crate's 500-line convention; the test module path stays
//! `watchdog_tests::trip::*`, so the ledger filter is unaffected.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use shared::{
  ActionStep, AgentJobRequestMessage, JobAuthorization, JobEndpoint, JobResources, ListenerEvent,
  RunnerConfig, RunnerError, SecretMasker, TaskOrchestrationPlanReference,
};

use crate::SessionCtx;
use crate::helpers::WatchdogConfig;
use crate::job_lifecycle::poll_and_execute;

/// Boxed error alias for helpers that use `?` — see the `mod retry` note
/// at the top of `watchdog_tests.rs`: `allow-expect-in-tests` only covers
/// `#[tokio::test]` fns themselves, not their helpers.
type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

/// A minimal-but-realistic `AgentJobRequestMessage`: one `SystemVssConnection`
/// endpoint carrying only a bearer (no cache/results/live-log URLs, so this
/// fixture's network surface stays exactly the five endpoints named in the
/// plan) and one script step.
fn job_message(server_uri: &str, job_id: &str, script: &str) -> AgentJobRequestMessage {
  AgentJobRequestMessage {
    message_type: "PipelineAgentJobRequest".to_owned(),
    plan: TaskOrchestrationPlanReference {
      scope_identifier: None,
      plan_id: "plan-1".to_owned(),
      plan_type: None,
      version: None,
    },
    timeline: None,
    job_id: job_id.to_owned(),
    job_display_name: "watchdog trip".to_owned(),
    job_name: "watchdog-trip".to_owned(),
    request_id: 42,
    locked_until: None,
    steps: vec![ActionStep::script("step-1", script, "")],
    variables: HashMap::new(),
    mask: Vec::new(),
    resources: JobResources {
      endpoints: vec![JobEndpoint {
        name: "SystemVssConnection".to_owned(),
        url: None,
        authorization: Some(JobAuthorization {
          scheme: "OAuth".to_owned(),
          parameters: HashMap::from([("AccessToken".to_owned(), "rs-token".to_owned())]),
        }),
        data: HashMap::new(),
      }],
    },
    run_service_url_field: Some(server_uri.to_owned()),
    context_data: HashMap::new(),
    workspace: None,
    environment_variables: Vec::new(),
    defaults: Vec::new(),
    file_table: Vec::new(),
  }
}

/// `/message`: the job once — `RunnerJobRequestBody`'s wire fields really
/// are snake_case (see `protocol::messages::RunnerJobRequestBody`) — then
/// 202 (no work) for every later poll; the mid-job `watch_for_gh_cancel`
/// watcher re-polls almost immediately after acquire.
async fn mount_message_poll(server: &MockServer, runner_request_id: &str, run_service_url: &str) {
  let body = serde_json::json!({
    "runner_request_id": runner_request_id,
    "run_service_url": run_service_url,
    "billing_owner_id": "billing-1",
  })
  .to_string();
  let envelope = serde_json::json!({
    "messageId": 1,
    "messageType": "RunnerJobRequest",
    "body": body,
    "iv": null,
  });
  Mock::given(method("GET"))
    .and(path("/message"))
    .respond_with(ResponseTemplate::new(200).set_body_json(envelope))
    .up_to_n_times(1)
    .mount(server)
    .await;
  Mock::given(method("GET"))
    .and(path("/message"))
    .respond_with(ResponseTemplate::new(202))
    .mount(server)
    .await;
}

/// `/acquirejob`: 200 with the job body plus the `x-plan-id` header
/// `acquire_job` reads off the response (`wire::net::run_service`).
async fn mount_acquire_job(
  server: &MockServer,
  plan_id: &str,
  msg: &AgentJobRequestMessage,
) -> TestResult<()> {
  let body = serde_json::to_value(msg)?;
  Mock::given(method("POST"))
    .and(path("/acquirejob"))
    .respond_with(
      ResponseTemplate::new(200)
        .insert_header("x-plan-id", plan_id)
        .set_body_json(body),
    )
    .mount(server)
    .await;
  Ok(())
}

/// `/acknowledge`: always 200 — best-effort, non-gating.
async fn mount_acknowledge(server: &MockServer) {
  Mock::given(method("POST"))
    .and(path("/acknowledge"))
    .respond_with(ResponseTemplate::new(200))
    .mount(server)
    .await;
}

/// `/renewjob`: always answers `status` — 503 classifies as
/// `RunnerError::Network` (feeds the watchdog; AC-2/AC-9), 401 classifies
/// as `RunnerError::Protocol` (definitive, never feeds it; AC-10).
async fn mount_renew_job_always(server: &MockServer, status: u16) {
  Mock::given(method("POST"))
    .and(path("/renewjob"))
    .respond_with(ResponseTemplate::new(status))
    .mount(server)
    .await;
}

/// `/completejob`: fails 500 for `fail_times` requests, then 200 forever —
/// `fail_times = 0` mounts a single always-200 mock (AC-2/AC-10);
/// `fail_times = 2` simulates the AC-9 reconnect.
async fn mount_complete_job(server: &MockServer, fail_times: u64) {
  if fail_times > 0 {
    Mock::given(method("POST"))
      .and(path("/completejob"))
      .respond_with(ResponseTemplate::new(500))
      .up_to_n_times(fail_times)
      .mount(server)
      .await;
  }
  Mock::given(method("POST"))
    .and(path("/completejob"))
    .respond_with(ResponseTemplate::new(200))
    .mount(server)
    .await;
}

fn make_config(dir: &tempfile::TempDir) -> RunnerConfig {
  RunnerConfig {
    data_dir: dir.path().join("data"),
    workspace_root: dir.path().join("work"),
    cgroup_path: None,
    ..RunnerConfig::default()
  }
}

/// Build a `SessionCtx` literally — every field is `pub(crate)` (see
/// `handler.rs`) — pointed at one `MockServer` for both the broker and the
/// Run Service. Drains `ctx.tx` in the background so the job's
/// `ListenerEvent` sends never backpressure on an unread receiver (the
/// real `GitHubListener::run` gives that role to the journal writer; this
/// fixture only needs the drain, not the journal).
fn make_ctx(
  server_uri: String,
  config: RunnerConfig,
  watchdog: WatchdogConfig,
) -> TestResult<SessionCtx> {
  let client = reqwest::Client::builder()
    .timeout(Duration::from_secs(5))
    .build()?;
  let (tx, mut rx) = mpsc::channel::<ListenerEvent>(256);
  tokio::spawn(async move { while rx.recv().await.is_some() {} });
  Ok(SessionCtx {
    client,
    token: "session-token".to_owned(),
    broker_url: server_uri,
    session_id: "session-1".to_owned(),
    config,
    masker: Arc::new(Mutex::new(SecretMasker::new())),
    cancel: CancellationToken::new(),
    tx,
    encryption_key: None,
    use_fips_encryption: false,
    rsa_private_key_der: Vec::new(),
    live_log: None,
    watchdog,
  })
}

/// Outcome of driving one `poll_and_execute` exchange against the fixture.
struct TripOutcome {
  result: Result<(), RunnerError>,
  elapsed: Duration,
  complete_requests: Vec<wiremock::Request>,
}

/// Wire up the fixture end to end and drive one real `poll_and_execute`
/// call, returning everything the ACs assert on.
async fn run_scenario(
  job_id: &str,
  script: &str,
  renew_status: u16,
  complete_fail_times: u64,
  outage_threshold: Duration,
  renew_interval: Duration,
) -> TestResult<TripOutcome> {
  let dir = tempfile::tempdir()?;
  let server = MockServer::start().await;
  let server_uri = server.uri();
  let msg = job_message(&server_uri, job_id, script);

  mount_message_poll(&server, &format!("rr-{job_id}"), &server_uri).await;
  mount_acquire_job(&server, "plan-1", &msg).await?;
  mount_acknowledge(&server).await;
  mount_renew_job_always(&server, renew_status).await;
  mount_complete_job(&server, complete_fail_times).await;

  let watchdog = WatchdogConfig {
    outage_threshold,
    renew_interval,
  };
  let mut ctx = make_ctx(server_uri, make_config(&dir), watchdog)?;

  let start = Instant::now();
  let result = poll_and_execute(&mut ctx).await;
  let elapsed = start.elapsed();

  let complete_requests = server
    .received_requests()
    .await
    .ok_or("request recording was disabled")?
    .into_iter()
    .filter(|r| r.url.path() == "/completejob")
    .collect();

  Ok(TripOutcome {
    result,
    elapsed,
    complete_requests,
  })
}

fn assert_ok(result: Result<(), RunnerError>) -> TestResult<()> {
  match result {
    Ok(()) => Ok(()),
    Err(e) => Err(format!("expected poll_and_execute Ok(()), got Err({e})").into()),
  }
}

fn assert_elapsed_under(elapsed: Duration, cap: Duration, context: &str) -> TestResult<()> {
  if elapsed >= cap {
    return Err(format!("{context}: elapsed {elapsed:?} >= cap {cap:?}").into());
  }
  Ok(())
}

fn assert_elapsed_at_least(elapsed: Duration, floor: Duration, context: &str) -> TestResult<()> {
  if elapsed < floor {
    return Err(format!("{context}: elapsed {elapsed:?} < floor {floor:?}").into());
  }
  Ok(())
}

fn assert_request_count(
  requests: &[wiremock::Request],
  expected: usize,
  what: &str,
) -> TestResult<()> {
  if requests.len() != expected {
    return Err(
      format!(
        "expected {expected} {what} request(s), got {}",
        requests.len()
      )
      .into(),
    );
  }
  Ok(())
}

fn annotations_mention_lost_connection(body: &serde_json::Value) -> TestResult<bool> {
  let annotations = body
    .get("annotations")
    .and_then(serde_json::Value::as_array)
    .ok_or("completejob body missing annotations array")?;
  Ok(annotations.iter().any(|a| {
    a.get("message")
      .and_then(serde_json::Value::as_str)
      .is_some_and(|m| m.contains("lost connection"))
  }))
}

fn body_conclusion(body: &serde_json::Value) -> TestResult<i64> {
  body
    .get("conclusion")
    .and_then(serde_json::Value::as_i64)
    .ok_or_else(|| format!("completejob body missing numeric conclusion: {body}").into())
}

/// Assert the completejob body carries `conclusion: Failure` (`3` —
/// `wire::reporting::ReportConclusion` is `serde_repr`, not a string; see
/// `crates/wire/src/reporting/types.rs`) and an annotation mentioning
/// "lost connection".
fn assert_failure_with_lost_connection(body: &serde_json::Value) -> TestResult<()> {
  let conclusion = body_conclusion(body)?;
  if conclusion != 3 {
    return Err(format!("expected conclusion 3 (Failure), got {conclusion}: {body}").into());
  }
  if !annotations_mention_lost_connection(body)? {
    return Err(format!("no annotation mentions 'lost connection': {body}").into());
  }
  Ok(())
}

/// AC-2: a renewal endpoint that always answers 503 trips the watchdog —
/// the in-flight `bash sleep 30` step is killed (job wall-clock far below
/// 30s), and the completion body reports `failure` + "lost connection".
#[tokio::test]
async fn ac2_outage_trips_kills_job_reports_failure() -> TestResult<()> {
  let outcome = tokio::time::timeout(
    Duration::from_secs(60),
    run_scenario(
      "job-ac2",
      "sleep 30",
      503,
      0,
      Duration::from_millis(300),
      Duration::from_millis(75),
    ),
  )
  .await
  .map_err(|_elapsed| "AC-2 scenario did not complete within the 60s test guard")??;

  assert_ok(outcome.result)?;
  assert_elapsed_under(
    outcome.elapsed,
    Duration::from_secs(20),
    "AC-2 kill latency",
  )?;
  assert_request_count(&outcome.complete_requests, 1, "completejob")?;
  let first = outcome
    .complete_requests
    .first()
    .ok_or("no completejob requests recorded")?;
  let body = first.body_json::<serde_json::Value>()?;
  assert_failure_with_lost_connection(&body)
}

/// AC-9: the same trip, but `/completejob` fails twice (503) before
/// succeeding — the report survives the simulated reconnect: 3 requests
/// received, the final body still carries `failure` + "lost connection",
/// and the overall outcome is `Ok`.
#[tokio::test]
async fn ac9_report_survives_reconnect() -> TestResult<()> {
  let outcome = tokio::time::timeout(
    Duration::from_secs(60),
    run_scenario(
      "job-ac9",
      "sleep 30",
      503,
      2,
      Duration::from_millis(300),
      Duration::from_millis(75),
    ),
  )
  .await
  .map_err(|_elapsed| "AC-9 scenario did not complete within the 60s test guard")??;

  assert_ok(outcome.result)?;
  assert_elapsed_under(
    outcome.elapsed,
    Duration::from_secs(20),
    "AC-9 kill latency",
  )?;
  assert_request_count(&outcome.complete_requests, 3, "completejob")?;
  let last = outcome
    .complete_requests
    .last()
    .ok_or("no completejob requests recorded")?;
  let body = last.body_json::<serde_json::Value>()?;
  assert_failure_with_lost_connection(&body)
}

/// AC-10: a renewal endpoint that answers 401 (definitive — `Protocol`,
/// not `Network`) for longer than the injected threshold must never trip
/// the watchdog — the short `bash sleep 1` step runs to normal completion
/// (`conclusion: success`, no "lost connection" annotation), and the job's
/// wall-clock sits near the step's own 1s sleep rather than the ~200ms
/// threshold — evidence it was not cancelled early.
#[tokio::test]
async fn ac10_definitive_renew_error_never_trips() -> TestResult<()> {
  let outcome = tokio::time::timeout(
    Duration::from_secs(60),
    run_scenario(
      "job-ac10",
      "sleep 1",
      401,
      0,
      Duration::from_millis(200),
      Duration::from_millis(50),
    ),
  )
  .await
  .map_err(|_elapsed| "AC-10 scenario did not complete within the 60s test guard")??;

  assert_ok(outcome.result)?;
  assert_elapsed_at_least(
    outcome.elapsed,
    Duration::from_millis(900),
    "AC-10 not-cancelled-early",
  )?;
  assert_request_count(&outcome.complete_requests, 1, "completejob")?;
  let first = outcome
    .complete_requests
    .first()
    .ok_or("no completejob requests recorded")?;
  let body = first.body_json::<serde_json::Value>()?;
  let conclusion = body_conclusion(&body)?;
  if conclusion != 2 {
    return Err(format!("expected conclusion 2 (Success), got {conclusion}: {body}").into());
  }
  if annotations_mention_lost_connection(&body)? {
    return Err(
      format!("unexpected 'lost connection' annotation on a non-tripped job: {body}").into(),
    );
  }
  Ok(())
}
