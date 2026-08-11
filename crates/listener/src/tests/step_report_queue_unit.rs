//! Unit + real-local-server tests for `step_report_queue`: `build_step_entry`
//! semantics (pure), AC-7 part a (batching under a held endpoint), and part
//! c at the queue level (bounded `drain` against an endpoint that never
//! responds). The end-to-end `poll_and_execute` drives for parts b/c live in
//! the sibling crate-root `tests/step_report_queue.rs` (declared in `lib.rs`
//! as `step_report_queue_test`), matching `startup_overlap.rs` / `early_ack.rs`.

use std::collections::HashMap as StdHashMap;
use std::time::Duration;

use axum::http::Method;

use shared::{Conclusion, RunnerEvent};
use wire::reporting::Status;

use super::*;
use crate::support::{Gate, RecordingServer, RouteResponse, wait_until};

// -- build_step_entry: pure event -> entry mapping ----------------------

#[test]
fn step_report_build_step_entry_for_step_started_populates_meta_and_in_progress_entry() {
  let mut step_meta = StepMetaMap::new();
  let event = RunnerEvent::StepStarted {
    step_id: "step-1".to_owned(),
    step_name: "Run tests".to_owned(),
    step_number: 2,
  };

  let entry = build_step_entry(&event, &mut step_meta).expect("StepStarted must map to an entry");

  assert_eq!(entry.external_id, "step-1");
  assert_eq!(entry.number, 2);
  assert_eq!(entry.name, "Run tests");
  assert_eq!(entry.status, Status::InProgress);
  assert_eq!(entry.conclusion, None);
  assert!(entry.started_at.is_some());
  assert_eq!(entry.completed_at, None);

  let meta = step_meta
    .get("step-1")
    .expect("StepStarted must record step_meta for the matching StepCompleted to recover");
  assert_eq!(meta.number, 2);
  assert_eq!(meta.name, "Run tests");
}

#[test]
fn step_report_build_step_entry_for_step_completed_uses_captured_meta() {
  let mut step_meta = StepMetaMap::new();
  let started = RunnerEvent::StepStarted {
    step_id: "step-1".to_owned(),
    step_name: "Run tests".to_owned(),
    step_number: 2,
  };
  build_step_entry(&started, &mut step_meta);

  let completed = RunnerEvent::StepCompleted {
    step_id: "step-1".to_owned(),
    conclusion: Conclusion::Success,
    outputs: std::collections::HashMap::new(),
  };
  let entry =
    build_step_entry(&completed, &mut step_meta).expect("StepCompleted must map to an entry");

  assert_eq!(entry.external_id, "step-1");
  assert_eq!(
    entry.number, 2,
    "must recover the number captured on StepStarted"
  );
  assert_eq!(entry.name, "Run tests");
  assert_eq!(entry.status, Status::Completed);
  assert_eq!(
    entry.conclusion,
    Some(wire::reporting::ReportConclusion::Success)
  );
  assert!(entry.started_at.is_some());
  assert!(entry.completed_at.is_some());
}

#[test]
fn step_report_build_step_entry_returns_none_for_unmapped_events() {
  let mut step_meta = StepMetaMap::new();
  let event = RunnerEvent::Log {
    step_id: "step-1".to_owned(),
    line: "hello".to_owned(),
    stream: shared::LogStream::Stdout,
  };
  assert!(build_step_entry(&event, &mut step_meta).is_none());
}

// -- AC-7 part a: batching under a held endpoint -------------------------

fn test_entry(id: &str, number: u32) -> wire::reporting::results_service::StepUpdateEntry {
  wire::reporting::results_service::StepUpdateEntry {
    external_id: id.to_owned(),
    number,
    name: format!("step-{number}"),
    status: Status::InProgress,
    conclusion: None,
    started_at: Some("2026-01-01T00:00:00Z".to_owned()),
    completed_at: None,
  }
}

/// AC-7 (a): with the endpoint's `WorkflowStepsUpdate` route held open, M
/// rapidly enqueued entries produce fewer than M requests once released,
/// with strictly increasing `change_order` per request and every entry
/// present (in order) across the batched request bodies.
#[tokio::test]
async fn step_report_queue_batches_rapid_entries_under_a_held_endpoint() {
  const WORKFLOW_STEPS_UPDATE_PATH: &str =
    "/twirp/github.actions.results.api.v1.WorkflowStepUpdateService/WorkflowStepsUpdate";
  const M: u32 = 6;

  let gate = Gate::new();
  let held_gate = gate.clone();
  let recording = RecordingServer::start(move |_base| {
    StdHashMap::from([(
      (Method::POST, WORKFLOW_STEPS_UPDATE_PATH.to_owned()),
      RouteResponse::ok_empty_held(held_gate),
    )])
  })
  .await;

  let (queue, _handle) = StepReportQueue::spawn(
    StepReportQueueConfig {
      client: reqwest::Client::new(),
      results_url: recording.base_url(),
      token: "step-report-token".to_owned(),
      run_backend_id: "run-1".to_owned(),
      job_backend_id: "job-1".to_owned(),
    },
    Duration::from_secs(10),
  );

  // No `.await` between these pushes: on the current-thread test runtime the
  // queue task cannot be polled until this fn yields, so all M entries land
  // in the channel before the task's first `recv` even runs — guaranteeing
  // one batch, the strongest form of "fewer than M requests".
  for i in 1..=M {
    queue.enqueue(test_entry(&format!("step-{i}"), i));
  }

  tokio::time::timeout(
    Duration::from_secs(5),
    wait_until(|| {
      !recording
        .requests_for(WORKFLOW_STEPS_UPDATE_PATH)
        .is_empty()
    }),
  )
  .await
  .expect("the batched flush should have reached the (held) recording server");

  gate.release();
  queue.drain().await;

  let requests = recording.requests_for(WORKFLOW_STEPS_UPDATE_PATH);
  assert!(
    requests.len() < M as usize,
    "expected fewer than {M} requests, got {}",
    requests.len()
  );

  // `WorkflowStepsUpdateRequest` is `Serialize`-only (it's a request body
  // this crate only ever sends, never receives), so the recorded bytes are
  // parsed as plain JSON here instead of back into that type.
  let mut seen_ids = Vec::new();
  let mut change_orders = Vec::new();
  for req in &requests {
    let parsed: serde_json::Value =
      serde_json::from_slice(&req.body).expect("recorded body must be valid JSON");
    change_orders.push(change_order_of(&parsed));
    seen_ids.extend(step_ids_of(&parsed));
  }

  assert!(
    is_strictly_increasing(&change_orders),
    "change_order must be strictly increasing per request, got {change_orders:?}"
  );
  let expected_ids: Vec<String> = (1..=M).map(|i| format!("step-{i}")).collect();
  assert_eq!(
    seen_ids, expected_ids,
    "every enqueued entry must appear, in order, across the batched request bodies"
  );
}

/// Extract `change_order` from a recorded `WorkflowStepsUpdate` body.
fn change_order_of(parsed: &serde_json::Value) -> u64 {
  parsed
    .get("change_order")
    .and_then(serde_json::Value::as_u64)
    .expect("change_order must be a u64")
}

/// Extract every `steps[].external_id` from a recorded `WorkflowStepsUpdate`
/// body, in array order.
fn step_ids_of(parsed: &serde_json::Value) -> Vec<String> {
  parsed
    .get("steps")
    .and_then(serde_json::Value::as_array)
    .expect("steps must be an array")
    .iter()
    .map(|step| {
      step
        .get("external_id")
        .and_then(serde_json::Value::as_str)
        .expect("external_id must be a string")
        .to_owned()
    })
    .collect()
}

/// True when `values` has no adjacent pair `values[i] >= values[i + 1]`.
fn is_strictly_increasing(values: &[u64]) -> bool {
  values
    .iter()
    .zip(values.iter().skip(1))
    .all(|(prev, next)| prev < next)
}

// -- AC-7 part c (queue level): bounded drain against a dead endpoint ----

/// AC-7 (c): `drain()` returns within its own injected deadline even when
/// every flush against the endpoint fails outright (connection refused —
/// nothing is listening on the port), rather than hanging the caller.
#[tokio::test]
async fn step_report_queue_drain_returns_within_its_deadline_when_the_endpoint_is_unreachable() {
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
    .await
    .expect("bind loopback listener");
  let addr = listener.local_addr().expect("read bound local addr");
  drop(listener); // nothing listening now — connects fail fast

  let short_deadline = Duration::from_millis(300);
  let (queue, _handle) = StepReportQueue::spawn(
    StepReportQueueConfig {
      client: reqwest::Client::new(),
      results_url: format!("http://{addr}"),
      token: "step-report-token".to_owned(),
      run_backend_id: "run-1".to_owned(),
      job_backend_id: "job-1".to_owned(),
    },
    short_deadline,
  );
  queue.enqueue(test_entry("step-1", 1));

  // A generous outer safety net: `drain` itself must return well inside
  // this, proving it is bounded by `short_deadline` and not left to the
  // outer timeout to rescue the test.
  tokio::time::timeout(Duration::from_secs(5), queue.drain())
    .await
    .expect("drain() must return on its own — it must not hang the caller");
}
