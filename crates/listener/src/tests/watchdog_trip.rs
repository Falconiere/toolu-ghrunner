//! End-to-end fixture driving the REAL `job_lifecycle::poll_and_execute`
//! against one wiremock `MockServer` standing in for both the broker
//! (`/message`, `/acknowledge`) and the Run Service (`/acquirejob`,
//! `/renewjob`, `/completejob`) — the job message's `run_service_url` field
//! points at the same server the broker calls use, matching the real
//! github.com JIT wire shape where both addresses come from the mint.
//! Covers AC-2 (trip → kill → report `failure`), AC-9 (trip +
//! report-on-reconnect), AC-10 (a persistent definitive renewal error must
//! never trip), plus the best-effort-ack contract (a failing `/acknowledge`
//! must not block the completion report). Filtered by the s6 ledger check
//! `test(/^watchdog_trip::/)` — the module was
//! `watchdog_tests::trip` before the tests moved into `tests/`.
//!
//! Also carries [`mod@override_rules`]: the pure unit tests for
//! `execution_loop::apply_outage_override` — the non-network fold of the
//! watchdog's trip flag into the job's final conclusion (no wiremock: plain
//! values in, plain values out). Both halves cover the same outage-trip
//! seam (end to end here, pure-fold in `override_rules`), which is why they
//! share this file.
//!
//! Originally split out of `watchdog_tests.rs` into its own nested-module
//! file (`mod trip;`) once the combined `retry` + `trip` file passed the
//! crate's 500-line convention; flattened again into
//! `crates/listener/src/tests/` (this file) so the whole crate's test code
//! lives under a sibling `tests/` tree instead of directly under `src/`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use shared::{
  ActionStep, AgentJobRequestMessage, Conclusion, JobAuthorization, JobEndpoint, JobResources,
  ListenerEvent, RunnerConfig, RunnerError, SecretMasker, TaskOrchestrationPlanReference,
};
use wire::reporting::ReportConclusion;

use crate::SessionCtx;
use crate::execution_loop::LOST_CONNECTION_MESSAGE;
use crate::helpers::WatchdogConfig;
use crate::job_lifecycle::poll_and_execute;

/// Boxed error alias for helpers that use `?` — see the module doc at the
/// top of `watchdog_retry.rs`: `allow-expect-in-tests` only covers
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
/// are `snake_case` (see `protocol::messages::RunnerJobRequestBody`) — then
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

/// `/acknowledge`: always answers `status`. The ack is best-effort and
/// non-gating, so a non-2xx here must not stop the completion report —
/// `ack_failure_does_not_block_completion` drives that with 500.
async fn mount_acknowledge(server: &MockServer, status: u16) {
  Mock::given(method("POST"))
    .and(path("/acknowledge"))
    .respond_with(ResponseTemplate::new(status))
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
/// fixture only needs the drain, not the journal). The drain task needs no
/// abort: `execute_with_renewal` clones `ctx.tx` twice (the event forwarder
/// and the renewal task) but joins both handles before returning, so the
/// `tx` moved into the returned `SessionCtx` is the last sender alive —
/// when that drops at the end of the test `rx.recv()` yields `None` and the
/// task returns on its own.
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
    job_log_upload: None,
    watchdog,
  })
}

/// Knobs for one fixture run. A struct rather than positional arguments
/// because the set grew past what a reader can track at the call site (and
/// past the 7-arg clippy cap). [`Scenario::new`] holds the ack-succeeds
/// default the watchdog ACs want; only the ack test overrides it.
struct Scenario {
  job_id: &'static str,
  script: &'static str,
  renew_status: u16,
  ack_status: u16,
  complete_fail_times: u64,
  outage_threshold: Duration,
  renew_interval: Duration,
}

impl Scenario {
  /// The watchdog-AC shape: `/acknowledge` answers 200 and `/completejob`
  /// succeeds on the first try.
  fn new(
    job_id: &'static str,
    script: &'static str,
    renew_status: u16,
    outage_threshold: Duration,
    renew_interval: Duration,
  ) -> Self {
    Self {
      job_id,
      script,
      renew_status,
      ack_status: 200,
      complete_fail_times: 0,
      outage_threshold,
      renew_interval,
    }
  }
}

/// Outcome of driving one `poll_and_execute` exchange against the fixture.
struct TripOutcome {
  result: Result<Option<Conclusion>, RunnerError>,
  elapsed: Duration,
  complete_requests: Vec<wiremock::Request>,
  acknowledge_requests: Vec<wiremock::Request>,
}

/// Wire up the fixture end to end and drive one real `poll_and_execute`
/// call, returning everything the ACs assert on.
async fn run_scenario(scenario: Scenario) -> TestResult<TripOutcome> {
  let dir = tempfile::tempdir()?;
  let server = MockServer::start().await;
  let server_uri = server.uri();
  let msg = job_message(&server_uri, scenario.job_id, scenario.script);

  mount_message_poll(&server, &format!("rr-{}", scenario.job_id), &server_uri).await;
  mount_acquire_job(&server, "plan-1", &msg).await?;
  mount_acknowledge(&server, scenario.ack_status).await;
  mount_renew_job_always(&server, scenario.renew_status).await;
  mount_complete_job(&server, scenario.complete_fail_times).await;

  let watchdog = WatchdogConfig {
    outage_threshold: scenario.outage_threshold,
    renew_interval: scenario.renew_interval,
  };
  let mut ctx = make_ctx(server_uri, make_config(&dir), watchdog)?;

  let start = Instant::now();
  let result = poll_and_execute(&mut ctx).await;
  let elapsed = start.elapsed();

  let requests = server
    .received_requests()
    .await
    .ok_or("request recording was disabled")?;
  let by_path = |want: &str| -> Vec<wiremock::Request> {
    requests
      .iter()
      .filter(|r| r.url.path() == want)
      .cloned()
      .collect()
  };

  Ok(TripOutcome {
    result,
    elapsed,
    complete_requests: by_path("/completejob"),
    acknowledge_requests: by_path("/acknowledge"),
  })
}

/// Assert `poll_and_execute` succeeded and hand back the conclusion it
/// reported, so callers can pin the verdict threaded up to
/// `GitHubListener::run` — not only the one written into the completejob
/// body.
fn assert_ok(result: Result<Option<Conclusion>, RunnerError>) -> TestResult<Option<Conclusion>> {
  match result {
    Ok(conclusion) => Ok(conclusion),
    Err(e) => Err(format!("expected poll_and_execute Ok, got Err({e})").into()),
  }
}

/// Assert the conclusion `poll_and_execute` returned is exactly `expected`.
fn assert_reported(
  result: Result<Option<Conclusion>, RunnerError>,
  expected: Conclusion,
) -> TestResult<()> {
  let got = assert_ok(result)?;
  if got != Some(expected) {
    return Err(format!("expected poll_and_execute to report {expected:?}, got {got:?}").into());
  }
  Ok(())
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

/// Find the annotation in a `/completejob` body whose `message` field is an
/// EXACT match for [`LOST_CONNECTION_MESSAGE`]. Field names are the real
/// wire shape `wire::reporting::Annotation` serializes to
/// (`#[serde(rename_all = "camelCase")]`, no per-field override on
/// `annotation_type`/`message` — see `crates/wire/src/reporting/types.rs`):
/// `annotationType` / `message`.
fn find_lost_connection_annotation(
  body: &serde_json::Value,
) -> TestResult<Option<&serde_json::Value>> {
  let annotations = body
    .get("annotations")
    .and_then(serde_json::Value::as_array)
    .ok_or("completejob body missing annotations array")?;
  Ok(annotations.iter().find(|a| {
    a.get("message")
      .and_then(serde_json::Value::as_str)
      .is_some_and(|m| m == LOST_CONNECTION_MESSAGE)
  }))
}

fn body_conclusion(body: &serde_json::Value) -> TestResult<i64> {
  body
    .get("conclusion")
    .and_then(serde_json::Value::as_i64)
    .ok_or_else(|| format!("completejob body missing numeric conclusion: {body}").into())
}

/// Assert the completejob body carries `conclusion: Failure` (the real
/// `wire::reporting::ReportConclusion::Failure` discriminant —
/// `ReportConclusion` is `serde_repr`, not a string; see
/// `crates/wire/src/reporting/types.rs`) and exactly the "lost connection"
/// annotation the watchdog trip emits, with `annotationType: "error"`.
fn assert_failure_with_lost_connection(body: &serde_json::Value) -> TestResult<()> {
  let conclusion = body_conclusion(body)?;
  let expected = ReportConclusion::Failure as i64;
  if conclusion != expected {
    return Err(
      format!("expected conclusion {expected} (Failure), got {conclusion}: {body}").into(),
    );
  }
  let annotation = find_lost_connection_annotation(body)?
    .ok_or_else(|| format!("no annotation with the exact lost-connection message: {body}"))?;
  let annotation_type = annotation
    .get("annotationType")
    .and_then(serde_json::Value::as_str)
    .ok_or_else(|| format!("annotation missing annotationType field: {annotation}"))?;
  if annotation_type != "error" {
    return Err(
      format!("expected annotationType \"error\", got {annotation_type:?}: {annotation}").into(),
    );
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
    run_scenario(Scenario::new(
      "job-ac2",
      "sleep 30",
      503,
      Duration::from_millis(300),
      Duration::from_millis(75),
    )),
  )
  .await
  .map_err(|_elapsed| "AC-2 scenario did not complete within the 60s test guard")??;

  assert_reported(outcome.result, Conclusion::Failure)?;
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
    run_scenario(Scenario {
      complete_fail_times: 2,
      ..Scenario::new(
        "job-ac9",
        "sleep 30",
        503,
        Duration::from_millis(300),
        Duration::from_millis(75),
      )
    }),
  )
  .await
  .map_err(|_elapsed| "AC-9 scenario did not complete within the 60s test guard")??;

  assert_reported(outcome.result, Conclusion::Failure)?;
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
    run_scenario(Scenario::new(
      "job-ac10",
      "sleep 1",
      401,
      Duration::from_millis(200),
      Duration::from_millis(50),
    )),
  )
  .await
  .map_err(|_elapsed| "AC-10 scenario did not complete within the 60s test guard")??;

  assert_reported(outcome.result, Conclusion::Success)?;
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
  let expected = ReportConclusion::Success as i64;
  if conclusion != expected {
    return Err(
      format!("expected conclusion {expected} (Success), got {conclusion}: {body}").into(),
    );
  }
  if find_lost_connection_annotation(&body)?.is_some() {
    return Err(
      format!("unexpected 'lost connection' annotation on a non-tripped job: {body}").into(),
    );
  }
  Ok(())
}

/// A failing `/acknowledge` must not doom the completion report: the ack
/// targets the broker while `complete_job` targets the Run Service, so
/// `acknowledge_best_effort` logs a WARN and `poll_and_execute` carries on.
///
/// Drives a job that never renews (a `true` step finishes long before the
/// 5s renew interval) against a 500 ack, and asserts the ack really was
/// attempted (one request) while the completion still landed once with
/// `conclusion: success`.
#[tokio::test]
async fn ack_failure_does_not_block_completion() -> TestResult<()> {
  let outcome = tokio::time::timeout(
    Duration::from_secs(60),
    run_scenario(Scenario {
      ack_status: 500,
      ..Scenario::new(
        "job-ack-fail",
        "true",
        200,
        Duration::from_secs(300),
        Duration::from_secs(5),
      )
    }),
  )
  .await
  .map_err(|_elapsed| "ack-failure scenario did not complete within the 60s test guard")??;

  assert_reported(outcome.result, Conclusion::Success)?;
  assert_request_count(&outcome.acknowledge_requests, 1, "acknowledge")?;
  assert_request_count(&outcome.complete_requests, 1, "completejob")?;
  let first = outcome
    .complete_requests
    .first()
    .ok_or("no completejob requests recorded")?;
  let body = first.body_json::<serde_json::Value>()?;
  let conclusion = body_conclusion(&body)?;
  let expected = ReportConclusion::Success as i64;
  if conclusion != expected {
    return Err(
      format!("expected conclusion {expected} (Success), got {conclusion}: {body}").into(),
    );
  }
  Ok(())
}

/// Pure unit tests for `execution_loop::apply_outage_override` — the
/// non-network fold of the watchdog's trip flag into the job's final
/// conclusion (no wiremock: plain values in, plain values out). Filtered
/// by the s6 ledger check `test(/^watchdog_trip::override_rules/)`.
mod override_rules {
  use shared::Conclusion;

  use crate::execution_loop::{LOST_CONNECTION_MESSAGE, apply_outage_override};

  /// (a) An untripped flag leaves the conclusion unchanged and adds no
  /// annotations, whatever the conclusion was.
  #[test]
  fn untripped_leaves_conclusion_and_annotations_unchanged() {
    for conclusion in [
      Conclusion::Success,
      Conclusion::Failure,
      Conclusion::Cancelled,
      Conclusion::Skipped,
    ] {
      let (out, annotations) = apply_outage_override(conclusion, false);
      assert_eq!(out, conclusion, "untripped must not change the conclusion");
      assert!(annotations.is_empty(), "untripped must add no annotations");
    }
  }

  /// (b) A tripped flag alongside a `Success` conclusion is the
  /// trip-during-teardown race (the job finished before the cancel
  /// landed) — it stays `Success`, with no annotation.
  #[test]
  fn tripped_success_stays_success_with_no_annotation() {
    let (conclusion, annotations) = apply_outage_override(Conclusion::Success, true);
    assert_eq!(conclusion, Conclusion::Success);
    assert!(annotations.is_empty());
  }

  /// (c) A tripped flag alongside a non-`Success` conclusion (e.g. a
  /// GH-initiated `Cancelled` racing the trip) overrides to `Failure` with
  /// exactly one `annotation_type: "error"` annotation carrying the exact
  /// production message.
  #[test]
  fn tripped_non_success_overrides_to_failure_with_error_annotation() {
    for original in [
      Conclusion::Failure,
      Conclusion::Cancelled,
      Conclusion::Skipped,
    ] {
      let (conclusion, annotations) = apply_outage_override(original, true);
      assert_eq!(conclusion, Conclusion::Failure);
      assert_eq!(annotations.len(), 1);
      let annotation = annotations.first().expect("checked len() == 1 above");
      assert_eq!(annotation.annotation_type, "error");
      assert_eq!(annotation.message, LOST_CONNECTION_MESSAGE);
    }
  }
}
