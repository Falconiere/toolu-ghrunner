//! S8 / AC-3: `acknowledge_best_effort` runs immediately after `acquire_job`
//! succeeds, not after the job completes (see the design note on
//! `job_lifecycle::acknowledge_best_effort`).
//!
//! Drives a REAL `poll_and_execute` against one wiremock `MockServer`
//! standing in for both the broker (`/message`, `/acknowledge`) and the Run
//! Service (`/acquirejob`, `/completejob`) — same shape as
//! `tests/watchdog_trip.rs`'s fixture. `MockServer::received_requests`
//! returns requests "in the order they were received" (wiremock's own doc,
//! `src/mock_server/bare_server.rs`), which is all AC-3 needs: the recorded
//! order must be acquire -> ack -> ... -> complete, and a failing
//! `/acknowledge` must still let the job succeed (single-attempt,
//! WARN-only, non-gating — unchanged by the move).

use std::collections::HashMap;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use shared::{
  ActionStep, AgentJobRequestMessage, Conclusion, JobAuthorization, JobEndpoint, JobResources,
  ListenerEvent, RunnerConfig, RunnerError, SecretMasker, TaskOrchestrationPlanReference,
};

use crate::SessionCtx;
use crate::helpers::WatchdogConfig;
use crate::job_lifecycle::poll_and_execute;

/// Boxed error alias for helpers that use `?` — `clippy::expect_used` /
/// `unwrap_used` are only allowed inside `#[tokio::test]` fns themselves
/// (`clippy.toml: allow-expect-in-tests`), not their helpers — see
/// `tests/watchdog_retry.rs`.
type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

/// A minimal `AgentJobRequestMessage`: one `SystemVssConnection` endpoint
/// carrying a bearer, no cache/results/live-log URLs, and one trivial
/// script step — this test's assertions are about broker request order,
/// not the engine's own network surface.
fn job_message(server_uri: &str, job_id: &str) -> AgentJobRequestMessage {
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
    job_display_name: "early ack".to_owned(),
    job_name: "early-ack".to_owned(),
    request_id: 42,
    locked_until: None,
    steps: vec![ActionStep::script("step-1", "true", "")],
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

/// `/message`: the job once, then 202 (no work) forever after — matches
/// `tests/watchdog_trip.rs::mount_message_poll`.
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
/// `acquire_job` reads off the response.
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

/// `/acknowledge`: always answers `status` — 500 exercises the WARN-only,
/// non-gating contract.
async fn mount_acknowledge(server: &MockServer, status: u16) {
  Mock::given(method("POST"))
    .and(path("/acknowledge"))
    .respond_with(ResponseTemplate::new(status))
    .mount(server)
    .await;
}

/// `/completejob`: always 200.
async fn mount_complete_job(server: &MockServer) {
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

/// Build a `SessionCtx` literally (every field is `pub(crate)`), pointed at
/// one `MockServer` for both the broker and the Run Service. Drains
/// `ctx.tx` in the background — see the identical rationale on
/// `tests/watchdog_trip.rs::make_ctx`. `WatchdogConfig::default` keeps the
/// prod 60s renewal cadence, well past this fixture's `true` step, so no
/// `/renewjob` route is needed.
fn make_ctx(server_uri: String, config: RunnerConfig) -> SessionCtx {
  let (tx, mut rx) = mpsc::channel::<ListenerEvent>(256);
  tokio::spawn(async move { while rx.recv().await.is_some() {} });
  SessionCtx {
    client: reqwest::Client::new(),
    token: "session-token".to_owned(),
    broker_url: server_uri,
    session_id: "session-1".to_owned(),
    config,
    masker: std::sync::Arc::new(std::sync::Mutex::new(SecretMasker::new())),
    cancel: CancellationToken::new(),
    tx,
    encryption_key: None,
    use_fips_encryption: false,
    rsa_private_key_der: Vec::new(),
    live_log: None,
    job_log_upload: None,
    watchdog: WatchdogConfig::default(),
  }
}

/// Outcome of driving one `poll_and_execute` exchange against the fixture.
struct Outcome {
  result: Result<Option<Conclusion>, RunnerError>,
  /// Every request the mock server received, in arrival order.
  requests: Vec<wiremock::Request>,
}

/// Wire up the fixture end to end (one job, `/acknowledge` answering
/// `ack_status`) and drive one real `poll_and_execute` call.
async fn run_scenario(job_id: &str, ack_status: u16) -> TestResult<Outcome> {
  let dir = tempfile::tempdir()?;
  let server = MockServer::start().await;
  let server_uri = server.uri();
  let msg = job_message(&server_uri, job_id);

  mount_message_poll(&server, &format!("rr-{job_id}"), &server_uri).await;
  mount_acquire_job(&server, "plan-1", &msg).await?;
  mount_acknowledge(&server, ack_status).await;
  mount_complete_job(&server).await;

  let mut ctx = make_ctx(server_uri, make_config(&dir));
  let result = poll_and_execute(&mut ctx).await;

  let requests = server
    .received_requests()
    .await
    .ok_or("request recording was disabled")?;

  Ok(Outcome { result, requests })
}

/// Assert `poll_and_execute` reported a successful conclusion.
fn assert_succeeded(result: &Result<Option<Conclusion>, RunnerError>) -> TestResult<()> {
  match result {
    Ok(Some(Conclusion::Success)) => Ok(()),
    other => Err(format!("expected Ok(Some(Success)), got {other:?}").into()),
  }
}

/// The index (in arrival order) of the first recorded request whose path
/// matches `want`.
fn first_index(requests: &[wiremock::Request], want: &str) -> TestResult<usize> {
  requests
    .iter()
    .position(|r| r.url.path() == want)
    .ok_or_else(|| format!("no recorded request for {want}").into())
}

/// AC-3: the recorded broker/Run-Service request order for a normal job is
/// acquire -> ack -> ... -> complete — the ack arrives strictly before the
/// completion report.
#[tokio::test]
async fn ack_arrives_before_completion_in_recorded_request_order() -> TestResult<()> {
  let outcome = run_scenario("job-early-ack-order", 200).await?;
  assert_succeeded(&outcome.result)?;

  let acquire_at = first_index(&outcome.requests, "/acquirejob")?;
  let ack_at = first_index(&outcome.requests, "/acknowledge")?;
  let complete_at = first_index(&outcome.requests, "/completejob")?;

  if !(acquire_at < ack_at && ack_at < complete_at) {
    return Err(
      format!(
        "expected acquire({acquire_at}) < ack({ack_at}) < complete({complete_at}) in recorded order"
      )
      .into(),
    );
  }
  Ok(())
}

/// AC-3: an ack endpoint returning 500 is still WARN-only and non-gating —
/// the ack is attempted exactly once and the job still completes
/// successfully.
#[tokio::test]
async fn ack_500_still_warns_and_job_completes_successfully() -> TestResult<()> {
  let outcome = run_scenario("job-early-ack-500", 500).await?;
  assert_succeeded(&outcome.result)?;

  let ack_count = outcome
    .requests
    .iter()
    .filter(|r| r.url.path() == "/acknowledge")
    .count();
  if ack_count != 1 {
    return Err(format!("expected exactly 1 acknowledge attempt, got {ack_count}").into());
  }

  let complete_count = outcome
    .requests
    .iter()
    .filter(|r| r.url.path() == "/completejob")
    .count();
  if complete_count != 1 {
    return Err(format!("expected exactly 1 completejob request, got {complete_count}").into());
  }
  Ok(())
}
