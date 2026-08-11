//! S10 / AC-2: the finalize split — the per-step upload drain still runs
//! before the conclusion (so every step keeps its `completedLogURL`), while
//! the combined job-level log upload overlaps `report_completion` and is
//! joined by `helpers::cleanup_session` before the listener returns.
//!
//! Drives a REAL `poll_and_execute` against: (a) a wiremock `MockServer`
//! standing in for the BROKER long-poll surface only (`/message`,
//! `/acknowledge`, the teardown `DELETE /session/…`) — it answers 202 in a
//! tight loop for the whole job, which would otherwise flood the recording
//! server's timeline — and (b) a [`crate::support::RecordingServer`]
//! standing in for BOTH the Run Service (`/acquirejob`, `/completejob`) and
//! the Results Service (Twirp RPCs + the signed blob URLs they hand out).
//! Putting the completion report and the log-blob traffic on one recorder
//! is what makes their arrival `Instant`s directly comparable.
//!
//! The discriminator for the overlap is the held `PUT /job-blob`: under the
//! OLD code the whole combined job-log upload (signed URL → blob PUT →
//! `CreateJobLogsMetadata`) completed inside `finalize_job_logs` BEFORE the
//! forwarder sent the conclusion, so holding the blob PUT would stall the
//! job forever and `poll_and_execute` could never reach `/completejob`.
//! Under the NEW split it returns with the upload still in flight.

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
  ListenerEvent, RunnerConfig, RunnerError, SecretMasker, TaskOrchestrationPlanReference,
  VariableValue,
};

use crate::SessionCtx;
use crate::helpers::{WatchdogConfig, cleanup_session};
use crate::job_lifecycle::poll_and_execute;
use crate::support::{Gate, RecordedRequest, RecordingServer, RouteResponse, wait_until};

/// Boxed error alias for helpers that use `?` — see the pattern in
/// `tests/watchdog_retry.rs`.
type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

/// Twirp path for `GetStepLogsSignedBlobURL` — mirrors the private
/// `RESULTS_RECEIVER_SERVICE` constant in `wire::net::results_service`.
const GET_STEP_SIGNED_URL_PATH: &str =
  "/twirp/results.services.receiver.Receiver/GetStepLogsSignedBlobURL";
/// Twirp path for `GetJobLogsSignedBlobURL`.
const GET_JOB_SIGNED_URL_PATH: &str =
  "/twirp/results.services.receiver.Receiver/GetJobLogsSignedBlobURL";
/// Twirp path for `CreateJobLogsMetadata` — the LAST of the combined
/// job-log upload's three round-trips, so its arrival is the "the upload
/// finished" marker this file asserts against.
const CREATE_JOB_LOGS_METADATA_PATH: &str =
  "/twirp/results.services.receiver.Receiver/CreateJobLogsMetadata";
/// Signed-blob path the per-step log uploads PUT to. Kept distinct from
/// [`JOB_BLOB_PATH`] so a hold on the job blob cannot also stall the
/// per-step drain — which runs BEFORE the conclusion and would deadlock.
const STEP_BLOB_PATH: &str = "/step-blob";
/// Signed-blob path the combined job-level log upload PUTs to.
const JOB_BLOB_PATH: &str = "/job-blob";

/// A real `AgentJobRequestMessage` pointed at `results_base_url` for both
/// the Run Service (`run_service_url_field`) and the Results Service
/// (`system.github.results_endpoint`), with explicit backend ids so the
/// Twirp requests carry real values rather than the `plan_id` fallback. No
/// `FeedStreamUrl`: the live-log overlap is `tests/startup_overlap.rs`'s AC.
///
/// The single step `echo`es, so it produces log lines and therefore a real
/// per-step blob upload — without output the streamer returns `None` and
/// the `completedLogURL` assertion would pass vacuously.
fn job_message(results_base_url: &str, job_id: &str) -> AgentJobRequestMessage {
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
    job_display_name: "finalize split".to_owned(),
    job_name: "finalize-split".to_owned(),
    request_id: 42,
    locked_until: None,
    steps: vec![ActionStep::script("step-1", "echo finalize-split-line", "")],
    variables: HashMap::from([
      (
        "system.github.results_endpoint".to_owned(),
        VariableValue {
          value: results_base_url.to_owned(),
          is_secret: false,
        },
      ),
      (
        "system.github.run_backend_id".to_owned(),
        VariableValue {
          value: "run-backend-1".to_owned(),
          is_secret: false,
        },
      ),
      (
        "system.github.job_backend_id".to_owned(),
        VariableValue {
          value: "job-backend-1".to_owned(),
          is_secret: false,
        },
      ),
    ]),
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
    run_service_url_field: Some(results_base_url.to_owned()),
    context_data: HashMap::new(),
    workspace: None,
    environment_variables: Vec::new(),
    defaults: Vec::new(),
    file_table: Vec::new(),
  }
}

/// The broker surface: `/message` hands out the job once then answers 202
/// forever, `/acknowledge` succeeds, and any `DELETE` (the teardown
/// `/session/<id>` call `cleanup_session` makes) succeeds.
async fn mount_broker(
  server: &MockServer,
  runner_request_id: &str,
  run_service_url: &str,
) -> TestResult<()> {
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
  Mock::given(method("POST"))
    .and(path("/acknowledge"))
    .respond_with(ResponseTemplate::new(200))
    .mount(server)
    .await;
  Mock::given(method("DELETE"))
    .respond_with(ResponseTemplate::new(200))
    .mount(server)
    .await;
  Ok(())
}

/// Route table for the recording Run + Results Service. `/acquirejob`
/// returns the job body (no `x-plan-id` header — `acquire_job` defaults it,
/// and the backend ids come from the message's own variables); the two
/// signed-blob RPCs hand out URLs back at this same server, on the two
/// DISTINCT blob paths. `job_blob_hold`, when set, holds the combined
/// job-log blob PUT open until the test releases it. Everything else
/// (`/completejob`, `WorkflowStepsUpdate`, both `Create*LogsMetadata`, the
/// step blob PUT) falls through to the recorder's default `200 {}` — still
/// recorded, just not configured.
fn service_routes(
  job_blob_hold: Option<Gate>,
  job_id: &str,
  base_url: &str,
) -> HashMap<(Method, String), RouteResponse> {
  let step_signed =
    json!({"logs_url": format!("{base_url}{STEP_BLOB_PATH}"), "blob_storage_type": "OTHER"});
  let job_signed =
    json!({"logs_url": format!("{base_url}{JOB_BLOB_PATH}"), "blob_storage_type": "OTHER"});
  let acquired = match serde_json::to_value(job_message(base_url, job_id)) {
    Ok(v) => v,
    // `RecordingServer::start` takes a plain `FnOnce`, so there is no `?`
    // here; an unserializable fixture would surface as an acquire-parse
    // failure, which the caller's `assert_succeeded` reports.
    Err(e) => json!({"fixture_serialization_error": e.to_string()}),
  };
  let mut routes = HashMap::from([
    (
      (Method::POST, "/acquirejob".to_owned()),
      RouteResponse::ok_json(&acquired),
    ),
    (
      (Method::POST, GET_STEP_SIGNED_URL_PATH.to_owned()),
      RouteResponse::ok_json(&step_signed),
    ),
    (
      (Method::POST, GET_JOB_SIGNED_URL_PATH.to_owned()),
      RouteResponse::ok_json(&job_signed),
    ),
  ]);
  if let Some(gate) = job_blob_hold {
    routes.insert(
      (Method::PUT, JOB_BLOB_PATH.to_owned()),
      RouteResponse::ok_empty_held(gate),
    );
  }
  routes
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

/// Everything a test wires up before driving `poll_and_execute`. `_dir` is
/// held only to keep the `TempDir` (and its workspace) alive.
struct Fixture {
  recording: RecordingServer,
  broker: MockServer,
  ctx: SessionCtx,
  _dir: tempfile::TempDir,
}

async fn build_fixture(job_id: &str, job_blob_hold: Option<Gate>) -> TestResult<Fixture> {
  let owned_job_id = job_id.to_owned();
  let recording =
    RecordingServer::start(move |base_url| service_routes(job_blob_hold, &owned_job_id, base_url))
      .await;

  let broker = MockServer::start().await;
  mount_broker(&broker, &format!("rr-{job_id}"), &recording.base_url()).await?;

  let dir = tempfile::tempdir()?;
  let ctx = make_ctx(broker.uri(), make_config(&dir));
  Ok(Fixture {
    recording,
    broker,
    ctx,
    _dir: dir,
  })
}

/// Assert `poll_and_execute` reported a successful conclusion.
fn assert_succeeded(result: &Result<Option<Conclusion>, RunnerError>) -> TestResult<()> {
  match result {
    Ok(Some(Conclusion::Success)) => Ok(()),
    other => Err(format!("expected Ok(Some(Success)), got {other:?}").into()),
  }
}

/// The single recorded request for `path`, or an error naming what was
/// missing.
fn one_request(recording: &RecordingServer, request_path: &str) -> TestResult<RecordedRequest> {
  recording
    .requests_for(request_path)
    .into_iter()
    .next()
    .ok_or_else(|| format!("no recorded request for {request_path}").into())
}

/// Wait (bounded) for at least one request to `request_path` to reach the
/// recording server, reporting `what` if it never does.
async fn wait_for_request(
  recording: &RecordingServer,
  request_path: &str,
  what: &'static str,
) -> TestResult<()> {
  tokio::time::timeout(
    Duration::from_secs(10),
    wait_until(|| !recording.requests_for(request_path).is_empty()),
  )
  .await
  .map_err(|_elapsed| what)?;
  Ok(())
}

/// Wait (bounded) for the teardown `DELETE /session/…` to reach the broker
/// — the observable proof that `cleanup_session` is already running and has
/// nothing left to do but join its handles.
async fn wait_for_session_delete(broker: &MockServer) -> TestResult<()> {
  tokio::time::timeout(Duration::from_secs(10), async {
    loop {
      if let Some(requests) = broker.received_requests().await
        && requests.iter().any(|r| r.method.as_str() == "DELETE")
      {
        return;
      }
      tokio::time::sleep(Duration::from_millis(5)).await;
    }
  })
  .await
  .map_err(|_elapsed| "cleanup_session never issued its session DELETE")?;
  Ok(())
}

/// AC-2 (overlap + join): with the combined job-log blob PUT held open,
/// `report_completion`'s `/completejob` request still arrives at the
/// recording server while the upload is unfinished, and
/// `cleanup_session` — the join site reached on BOTH the `Ok` and the `Err`
/// path of `poll_and_execute` — does not return until the upload's final
/// `CreateJobLogsMetadata` call has landed.
#[tokio::test]
async fn finalize_split_completes_the_job_while_the_job_log_upload_is_held() -> TestResult<()> {
  let gate = Gate::new();
  let Fixture {
    recording,
    broker,
    mut ctx,
    _dir,
  } = build_fixture("job-finalize-overlap", Some(gate.clone())).await?;

  let job = tokio::spawn(async move {
    let result = poll_and_execute(&mut ctx).await;
    (result, ctx)
  });
  let (result, mut ctx) = tokio::time::timeout(Duration::from_secs(30), job)
    .await
    .map_err(|_elapsed| {
      "poll_and_execute never returned while the combined job-log blob PUT was held — the \
       upload is still blocking the conclusion instead of overlapping it"
    })?
    .map_err(|e| format!("poll_and_execute task panicked: {e}"))?;
  assert_succeeded(&result)?;

  wait_for_request(
    &recording,
    JOB_BLOB_PATH,
    "the combined job-log blob PUT never reached the recording server",
  )
  .await?;
  let complete = one_request(&recording, "/completejob")?;
  if !recording
    .requests_for(CREATE_JOB_LOGS_METADATA_PATH)
    .is_empty()
  {
    return Err(
      "the job-log upload finalized while its blob PUT was still held — the hold is not \
       actually holding, so this test proves nothing"
        .into(),
    );
  }

  // `cleanup_session` runs concurrently; release the hold only once its
  // session DELETE proves it is underway, so a version that did NOT join
  // the upload handle would return before the upload's last round-trip.
  let cleanup = tokio::spawn(async move {
    cleanup_session(&mut ctx).await;
    Instant::now()
  });
  wait_for_session_delete(&broker).await?;
  gate.release();
  let returned_at = tokio::time::timeout(Duration::from_secs(30), cleanup)
    .await
    .map_err(|_elapsed| "cleanup_session never returned after the job-log upload was released")?
    .map_err(|e| format!("cleanup_session task panicked: {e}"))?;

  wait_for_request(
    &recording,
    CREATE_JOB_LOGS_METADATA_PATH,
    "the combined job-log upload never finalized after its blob PUT was released",
  )
  .await?;
  let metadata = one_request(&recording, CREATE_JOB_LOGS_METADATA_PATH)?;
  if complete.arrived_at >= metadata.arrived_at {
    return Err(
      "the completion report did not arrive before the combined job-log upload finished — \
       the upload is not overlapping report_completion"
        .into(),
    );
  }
  if metadata.arrived_at > returned_at {
    return Err(
      "cleanup_session returned before the combined job-log upload finished — the spawned \
       upload's JoinHandle is not being awaited"
        .into(),
    );
  }
  Ok(())
}

/// AC-2 (the finalize-split invariant): EVERY step in the recorded
/// `complete_job` body carries a non-null `completedLogURL`, so the per-step
/// upload drain — the half of the old `finalize_job_logs` that stayed on the
/// pre-conclusion path — still backfills the collector before
/// `report_completion` ships its `step_results`. Dropping it, or spawning it
/// detached alongside the job-log upload, leaves the field absent and GitHub
/// renders "log not found" for that step (verified: removing the
/// `drain_step_uploads` call fails this test on the script step).
#[tokio::test]
async fn finalize_split_keeps_a_log_url_on_every_completed_step() -> TestResult<()> {
  let Fixture {
    recording,
    broker: _broker,
    mut ctx,
    _dir,
  } = build_fixture("job-finalize-log-urls", None).await?;

  let result = tokio::time::timeout(Duration::from_secs(30), poll_and_execute(&mut ctx))
    .await
    .map_err(|_elapsed| "poll_and_execute did not complete within the safety timeout")?;
  assert_succeeded(&result)?;
  // Join the spawned job-log upload before the fixture drops, exactly as
  // `handler::GitHubListener::run` does.
  cleanup_session(&mut ctx).await;

  let complete = one_request(&recording, "/completejob")?;
  let body: serde_json::Value = serde_json::from_slice(&complete.body)?;
  let steps = body
    .get("stepResults")
    .and_then(serde_json::Value::as_array)
    .ok_or("expected a stepResults array in the recorded complete_job body")?;
  // "Set up job" plus the one script step: fewer means the fixture stopped
  // exercising the drain and the loop below would pass vacuously.
  if steps.len() < 2 {
    return Err(format!("expected at least 2 step results, got {}", steps.len()).into());
  }
  for step in steps {
    let name = step
      .get("name")
      .and_then(serde_json::Value::as_str)
      .unwrap_or("<unnamed>");
    match step
      .get("completedLogURL")
      .and_then(serde_json::Value::as_str)
    {
      Some(url) if !url.is_empty() => {},
      _ => {
        return Err(
          format!("step {name:?} has no completedLogURL in the complete_job body: {step}").into(),
        );
      },
    }
  }
  Ok(())
}
