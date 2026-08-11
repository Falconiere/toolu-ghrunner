//! S9 / AC-7 (parts b, c): end-to-end `poll_and_execute` drives proving the
//! [`crate::step_report_queue::StepReportQueue`] wiring's two job-level
//! guarantees:
//!
//! - **Healthy endpoint (b):** every `WorkflowStepsUpdate` request — the
//!   setup step's own direct report AND every batch the queue flushes —
//!   arrives (in recorded request order) before `report_completion`'s
//!   `/completejob` request. One `wiremock::MockServer` stands in for BOTH
//!   the broker/Run Service AND the Results Service, so `received_requests`
//!   gives one arrival-ordered list across both surfaces (matching this
//!   crate's `tests/early_ack.rs::ack_arrives_before_completion...` — an
//!   order fact, not a timing one, so plain wiremock suffices per the
//!   design's own AC-3 note).
//! - **Endpoint down (c):** a `results_endpoint` pointed at a closed local
//!   port (nothing listening — the connection is refused immediately) still
//!   lets the job reach a `Success` conclusion and report it. This proves
//!   `StepReportQueue::drain` does not hang the forwarder even when every
//!   flush fails, without needing the production 10s deadline to actually
//!   elapse (a held-forever endpoint would work too but would slow this
//!   test down for no extra coverage — [`crate::step_report_queue`]'s own
//!   sibling unit tests cover the deadline itself with an injected short
//!   one).
//!
//! Batching under a HELD endpoint (AC-7 part a) and the bounded-deadline
//! proof are queue-level, not end-to-end — see
//! `step_report_queue::tests` (declared from `step_report_queue.rs`).

use std::collections::HashMap;

use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use shared::{
  ActionStep, AgentJobRequestMessage, Conclusion, JobAuthorization, JobEndpoint, JobResources,
  ListenerEvent, RunnerConfig, SecretMasker, TaskOrchestrationPlanReference, VariableValue,
};

use crate::SessionCtx;
use crate::helpers::WatchdogConfig;
use crate::job_lifecycle::poll_and_execute;

/// Boxed error alias for helpers that use `?` — see the pattern in
/// `tests/watchdog_retry.rs`.
type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

/// Twirp path for `WorkflowStepsUpdate` — mirrors the private
/// `WORKFLOW_STEP_UPDATE_SERVICE` constant in `wire::net::results_service`.
const WORKFLOW_STEPS_UPDATE_PATH: &str =
  "/twirp/github.actions.results.api.v1.WorkflowStepUpdateService/WorkflowStepsUpdate";

/// A real `AgentJobRequestMessage`: one `SystemVssConnection` endpoint
/// carrying a bearer, a `system.github.results_endpoint` variable, and one
/// trivial script step. No `FeedStreamUrl` — this file's ACs are about
/// Results Service / broker request ordering, not the live-log overlap
/// `tests/startup_overlap.rs` already covers.
fn job_message(broker_uri: &str, results_endpoint: &str, job_id: &str) -> AgentJobRequestMessage {
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
    job_display_name: "step report queue".to_owned(),
    job_name: "step-report-queue".to_owned(),
    request_id: 42,
    locked_until: None,
    steps: vec![ActionStep::script("step-1", "true", "")],
    variables: HashMap::from([(
      "system.github.results_endpoint".to_owned(),
      VariableValue {
        value: results_endpoint.to_owned(),
        is_secret: false,
      },
    )]),
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
    run_service_url_field: Some(broker_uri.to_owned()),
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

/// `/acquirejob`, `/acknowledge`, `/completejob` — always succeed.
async fn mount_broker(
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
  Mock::given(method("POST"))
    .and(path("/acknowledge"))
    .respond_with(ResponseTemplate::new(200))
    .mount(server)
    .await;
  Mock::given(method("POST"))
    .and(path("/completejob"))
    .respond_with(ResponseTemplate::new(200))
    .mount(server)
    .await;
  Ok(())
}

/// `WorkflowStepsUpdate`: always a plain `200 {}` — no hold. Everything
/// else the log-upload path might call (signed-URL RPCs, the blob PUT,
/// `Create*LogsMetadata`) is deliberately left unmocked: those calls are
/// best-effort (`log_uploader::upload.rs` WARNs and returns `None` on any
/// failure, including wiremock's default 404 for an unmatched request),
/// so a 404 there doesn't fail the job and keeps this fixture focused on
/// the one RPC this file's ACs are about.
async fn mount_workflow_steps_update(server: &MockServer) {
  Mock::given(method("POST"))
    .and(path(WORKFLOW_STEPS_UPDATE_PATH))
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

/// Build a `SessionCtx` literally (every field is `pub(crate)`). Drains
/// `ctx.tx` in the background — see the identical rationale on
/// `tests/watchdog_trip.rs::make_ctx`.
fn make_ctx(broker_uri: String, config: RunnerConfig) -> SessionCtx {
  let (tx, mut rx) = mpsc::channel::<ListenerEvent>(256);
  tokio::spawn(async move { while rx.recv().await.is_some() {} });
  SessionCtx {
    client: reqwest::Client::new(),
    token: "session-token".to_owned(),
    broker_url: broker_uri,
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

/// A local address with nothing listening: bind, read the address, then
/// drop the listener — the port is free again but any connect to it is
/// refused immediately (no listen backlog), standing in for "the Results
/// Service is down" without the test having to wait out a real timeout.
async fn unreachable_addr() -> TestResult<String> {
  let listener = TcpListener::bind("127.0.0.1:0").await?;
  let addr = listener.local_addr()?;
  drop(listener);
  Ok(format!("http://{addr}"))
}

/// AC-7 (b): on a healthy Results Service, every recorded
/// `WorkflowStepsUpdate` request (the setup step's own report, plus
/// whatever the step-report queue flushed for the real step) arrives
/// before `/completejob` — the `StepReportQueue::drain` call inside the
/// forwarder actually blocks the conclusion, not just in isolation.
#[tokio::test]
async fn step_report_queue_updates_arrive_before_completion_on_a_healthy_endpoint() -> TestResult<()>
{
  let server = MockServer::start().await;
  let server_uri = server.uri();
  let msg = job_message(&server_uri, &server_uri, "job-step-report-healthy");

  mount_message_poll(&server, "rr-step-report-healthy", &server_uri).await;
  mount_broker(&server, "plan-1", &msg).await?;
  mount_workflow_steps_update(&server).await;

  let dir = tempfile::tempdir()?;
  let mut ctx = make_ctx(server_uri, make_config(&dir));

  let outcome = poll_and_execute(&mut ctx).await;
  match &outcome {
    Ok(Some(Conclusion::Success)) => {},
    other => return Err(format!("expected Ok(Some(Success)), got {other:?}").into()),
  }

  let requests = server
    .received_requests()
    .await
    .ok_or("request recording was disabled")?;

  let last_step_update = requests
    .iter()
    .rposition(|r| r.url.path() == WORKFLOW_STEPS_UPDATE_PATH)
    .ok_or("expected at least one WorkflowStepsUpdate request")?;
  let complete_at = requests
    .iter()
    .position(|r| r.url.path() == "/completejob")
    .ok_or("expected a completejob request")?;

  if last_step_update >= complete_at {
    return Err(
      format!(
        "expected the last WorkflowStepsUpdate request ({last_step_update}) to arrive before \
       completejob ({complete_at}) in recorded order"
      )
      .into(),
    );
  }
  Ok(())
}

/// AC-7 (c): a Results Service that is unreachable for the whole job still
/// lets `poll_and_execute` reach a `Success` conclusion and report it —
/// `StepReportQueue::drain` returns instead of hanging the forwarder when
/// every flush it attempts fails.
#[tokio::test]
async fn step_report_queue_endpoint_down_still_completes_the_job() -> TestResult<()> {
  let broker = MockServer::start().await;
  let broker_uri = broker.uri();
  let results_endpoint = unreachable_addr().await?;
  let msg = job_message(&broker_uri, &results_endpoint, "job-step-report-down");

  mount_message_poll(&broker, "rr-step-report-down", &broker_uri).await;
  mount_broker(&broker, "plan-1", &msg).await?;

  let dir = tempfile::tempdir()?;
  let mut ctx = make_ctx(broker_uri, make_config(&dir));

  let outcome = tokio::time::timeout(std::time::Duration::from_secs(20), async {
    poll_and_execute(&mut ctx).await
  })
  .await
  .map_err(|_elapsed| {
    "poll_and_execute did not complete within the safety timeout — StepReportQueue::drain \
     appears to have hung instead of returning bounded"
  })?;

  match outcome {
    Ok(Some(Conclusion::Success)) => {},
    other => return Err(format!("expected Ok(Some(Success)), got {other:?}").into()),
  }

  let requests = broker
    .received_requests()
    .await
    .ok_or("request recording was disabled")?;
  let complete_count = requests
    .iter()
    .filter(|r| r.url.path() == "/completejob")
    .count();
  if complete_count != 1 {
    return Err(format!("expected exactly 1 completejob request, got {complete_count}").into());
  }
  Ok(())
}
