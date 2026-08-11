//! S7 / AC-1: `connect_live_log` and `report_setup_step` run concurrently
//! (`tokio::join!` inside `execute_with_renewal`) instead of strictly in
//! sequence.
//!
//! Drives a REAL `poll_and_execute` against: (a) a wiremock `MockServer`
//! standing in for the broker + Run Service (message/acquire/acknowledge/
//! complete — order only, matching this crate's wiremock convention, same
//! as `tests/watchdog_trip.rs`), (b) a [`crate::support::RecordingServer`]
//! standing in for the Results Service, whose `WorkflowStepsUpdate` route is
//! held behind a [`crate::support::Gate`] so the setup-step report cannot
//! complete until the test releases it, and (c) a
//! [`crate::support::WsServer`] standing in for the live-log `FeedStreamUrl`
//! WebSocket, whose handshake is held behind its OWN gate.
//!
//! The two gates are the discriminator: under the OLD strictly-serial code
//! (`connect_live_log().await` in full, THEN `report_setup_step().await`),
//! holding the WS handshake would make the setup-step call UNREACHABLE —
//! `report_setup_step` is never even invoked until `connect_live_log`
//! resolves, so it would never arrive at the recording server, and the
//! bounded wait in [`wait_for_setup_call_while_ws_held`] would time out.
//! Under the NEW `tokio::join!` code the setup-step call arrives
//! immediately regardless of the still-open WS gate, proving the two run
//! concurrently — an ordering/arrival fact, not a wall-clock threshold.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use axum::http::Method;
use serde_json::json;
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
use crate::support::{Gate, RecordingServer, RouteResponse, WsServer, wait_until};

/// Boxed error alias for helpers that use `?` — see the pattern in
/// `tests/watchdog_retry.rs`.
type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

/// Twirp path for `WorkflowStepsUpdate` — hit by BOTH the setup-step report
/// and every real step's `StepStarted`/`StepCompleted` report (same RPC,
/// see `crate::helpers::report_step_to_results` and
/// `crate::setup_step::report_setup_step`). Mirrors the private
/// `WORKFLOW_STEP_UPDATE_SERVICE` constant in `wire::net::results_service`.
const WORKFLOW_STEPS_UPDATE_PATH: &str =
  "/twirp/github.actions.results.api.v1.WorkflowStepUpdateService/WorkflowStepsUpdate";
/// Twirp path for `GetStepLogsSignedBlobURL` — mirrors the private
/// `RESULTS_RECEIVER_SERVICE` constant in `wire::net::results_service`.
const GET_STEP_SIGNED_URL_PATH: &str =
  "/twirp/results.services.receiver.Receiver/GetStepLogsSignedBlobURL";
/// Twirp path for `GetJobLogsSignedBlobURL`.
const GET_JOB_SIGNED_URL_PATH: &str =
  "/twirp/results.services.receiver.Receiver/GetJobLogsSignedBlobURL";

/// A real `AgentJobRequestMessage`: one `SystemVssConnection` endpoint
/// carrying a bearer AND a `FeedStreamUrl` (unlike `tests/watchdog_trip.rs`'s
/// fixture, which deliberately omits both results/live-log endpoints), a
/// `system.github.results_endpoint` variable pointing at the recording
/// server, and one trivial script step.
fn job_message(
  broker_uri: &str,
  results_base_url: &str,
  feed_stream_url: &str,
  job_id: &str,
) -> AgentJobRequestMessage {
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
    job_display_name: "startup overlap".to_owned(),
    job_name: "startup-overlap".to_owned(),
    request_id: 42,
    locked_until: None,
    steps: vec![ActionStep::script("step-1", "true", "")],
    variables: HashMap::from([(
      "system.github.results_endpoint".to_owned(),
      VariableValue {
        value: results_base_url.to_owned(),
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
        data: HashMap::from([("FeedStreamUrl".to_owned(), feed_stream_url.to_owned())]),
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

/// `/message`: the job once, then 202 (no work) forever after.
async fn mount_message_poll(server: &MockServer, runner_request_id: &str, run_service_url: &str) {
  let body = json!({
    "runner_request_id": runner_request_id,
    "run_service_url": run_service_url,
    "billing_owner_id": "billing-1",
  })
  .to_string();
  let envelope = json!({
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

/// `/acquirejob`, `/acknowledge`, `/completejob` — always succeed; this
/// test's ACs are about the Results Service / WS overlap, not broker order.
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

fn make_config(dir: &tempfile::TempDir) -> RunnerConfig {
  RunnerConfig {
    data_dir: dir.path().join("data"),
    workspace_root: dir.path().join("work"),
    cgroup_path: None,
    ..RunnerConfig::default()
  }
}

/// Build a `SessionCtx` literally (every field is `pub(crate)`), pointed at
/// the broker `MockServer`. Drains `ctx.tx` in the background — see the
/// identical rationale on `tests/watchdog_trip.rs::make_ctx`.
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

/// Route table for the recording Results Service: the setup-step
/// `WorkflowStepsUpdate` call is held behind `setup_gate`; the two
/// signed-blob-URL RPCs answer immediately with a URL back at this same
/// server (any other path, including the blob PUT and both
/// `Create*LogsMetadata` calls, falls through to the recorder's default
/// `200 {}`).
fn results_routes(setup_gate: Gate, base_url: &str) -> HashMap<(Method, String), RouteResponse> {
  let signed = json!({"logs_url": format!("{base_url}/blob"), "blob_storage_type": "OTHER"});
  HashMap::from([
    (
      (Method::POST, WORKFLOW_STEPS_UPDATE_PATH.to_owned()),
      RouteResponse::ok_empty_held(setup_gate),
    ),
    (
      (Method::POST, GET_STEP_SIGNED_URL_PATH.to_owned()),
      RouteResponse::ok_json(&signed),
    ),
    (
      (Method::POST, GET_JOB_SIGNED_URL_PATH.to_owned()),
      RouteResponse::ok_json(&signed),
    ),
  ])
}

/// Everything the test wires up before driving `poll_and_execute`. Holds
/// `_broker` / `_dir` only to keep them alive for the test's duration —
/// `MockServer::drop` tears down its listener, and the `TempDir` removes
/// its directory on drop.
struct Fixture {
  ws_server: WsServer,
  ws_gate: Gate,
  setup_gate: Gate,
  recording: RecordingServer,
  ctx: SessionCtx,
  _broker: MockServer,
  _dir: tempfile::TempDir,
}

async fn build_fixture() -> TestResult<Fixture> {
  let ws_gate = Gate::new();
  let ws_server = WsServer::start(ws_gate.clone()).await;

  let setup_gate = Gate::new();
  let recording =
    RecordingServer::start(|base_url| results_routes(setup_gate.clone(), base_url)).await;

  let broker = MockServer::start().await;
  let broker_uri = broker.uri();
  let msg = job_message(
    &broker_uri,
    &recording.base_url(),
    &ws_server.feed_stream_url_http(),
    "job-startup-overlap",
  );
  mount_message_poll(&broker, "rr-startup-overlap", &broker_uri).await;
  mount_broker(&broker, "plan-1", &msg).await?;

  let dir = tempfile::tempdir()?;
  let ctx = make_ctx(broker_uri, make_config(&dir));

  Ok(Fixture {
    ws_server,
    ws_gate,
    setup_gate,
    recording,
    ctx,
    _broker: broker,
    _dir: dir,
  })
}

/// Wait (bounded) for the setup-step call to reach the recording server,
/// then assert the WS handshake has NOT completed yet — the overlap proof.
/// See the module doc for why this is the regression discriminator.
async fn wait_for_setup_call_while_ws_held(
  recording: &RecordingServer,
  ws: &WsServer,
) -> TestResult<()> {
  tokio::time::timeout(
    Duration::from_secs(5),
    wait_until(|| {
      !recording
        .requests_for(WORKFLOW_STEPS_UPDATE_PATH)
        .is_empty()
    }),
  )
  .await
  .map_err(|_elapsed| {
    "the setup-step report never reached the recording server while the live-log \
     WS handshake was still held — connect_live_log and report_setup_step are not \
     running concurrently"
  })?;

  let setup_call = recording
    .requests_for(WORKFLOW_STEPS_UPDATE_PATH)
    .into_iter()
    .next()
    .ok_or("expected the setup-step WorkflowStepsUpdate request to be recorded")?;
  if setup_call.method != Method::POST {
    return Err(format!("expected a POST, got {}", setup_call.method).into());
  }
  if setup_call.body.is_empty() {
    return Err("expected the setup-step request to carry a JSON body".into());
  }

  if ws.handshake_completed_at().is_some() {
    return Err(
      "WS handshake already completed before the test released its gate — \
       the overlap window closed before we could observe it"
        .into(),
    );
  }
  if ws.tcp_accepted_at().is_none() {
    return Err("expected the WS upgrade attempt to have arrived (TCP accepted) by now".into());
  }
  Ok(())
}

/// Release both gates, then wait (bounded) for the WS handshake to
/// complete and the real step's `StepStarted` report (the SECOND
/// `WorkflowStepsUpdate` call) to arrive, asserting it arrived after
/// release.
async fn release_and_wait_for_engine_step(
  recording: &RecordingServer,
  ws: &WsServer,
  setup_gate: &Gate,
  ws_gate: &Gate,
) -> TestResult<()> {
  let release_instant = Instant::now();
  setup_gate.release();
  ws_gate.release();

  tokio::time::timeout(
    Duration::from_secs(5),
    wait_until(|| ws.handshake_completed_at().is_some()),
  )
  .await
  .map_err(|_elapsed| "WS handshake never completed after releasing its gate")?;

  tokio::time::timeout(
    Duration::from_secs(10),
    wait_until(|| recording.requests_for(WORKFLOW_STEPS_UPDATE_PATH).len() >= 2),
  )
  .await
  .map_err(|_elapsed| "the real step's StepStarted report never arrived")?;

  let step_started = recording
    .requests_for(WORKFLOW_STEPS_UPDATE_PATH)
    .into_iter()
    .nth(1)
    .ok_or("expected a second WorkflowStepsUpdate request")?;
  if step_started.arrived_at < release_instant {
    return Err(
      "the real step's report arrived before the setup-step/live-log gates were released".into(),
    );
  }
  Ok(())
}

/// AC-1: the WS handshake is held (via `ws_gate`) while the setup-step
/// report already reaches the recording server — proving `connect_live_log`
/// and `report_setup_step` run concurrently, not in sequence — and the
/// job's first REAL step report arrives only once both have resolved.
#[tokio::test]
async fn startup_overlap_runs_connect_live_log_and_setup_step_concurrently() -> TestResult<()> {
  let Fixture {
    ws_server,
    ws_gate,
    setup_gate,
    recording,
    mut ctx,
    _broker,
    _dir,
  } = build_fixture().await?;

  let job_handle = tokio::spawn(async move { poll_and_execute(&mut ctx).await });

  wait_for_setup_call_while_ws_held(&recording, &ws_server).await?;
  release_and_wait_for_engine_step(&recording, &ws_server, &setup_gate, &ws_gate).await?;

  let outcome = tokio::time::timeout(Duration::from_secs(20), job_handle)
    .await
    .map_err(|_elapsed| "poll_and_execute did not complete within the safety timeout")?
    .map_err(|e| format!("poll_and_execute task panicked: {e}"))?;

  match outcome {
    Ok(Some(Conclusion::Success)) => Ok(()),
    Ok(other) => Err(format!("expected Ok(Some(Success)), got Ok({other:?})").into()),
    Err(e) => Err(format!("expected poll_and_execute to succeed, got Err({e})").into()),
  }
}
