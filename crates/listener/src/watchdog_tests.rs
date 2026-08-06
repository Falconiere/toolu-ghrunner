//! In-crate integration tests for private seams unreachable from
//! `crates/listener/tests/` (`retry_transient`, `SessionCtx`) — the
//! `helpers.rs` precedent (see
//! docs/toolu/specs/2026-08-06-outage-watchdog-design.md "Test home").
//!
//! `mod retry` covers the `retry_transient` ACs (AC-3, AC-4, AC-7, AC-11);
//! filtered by the s3 ledger check `test(/^watchdog_tests::retry/)`.
//!
//! `mod trip` drives the REAL `job_lifecycle::poll_and_execute` end to end
//! against one wiremock `MockServer` (AC-2, AC-9, AC-10); filtered by the
//! s6 ledger check `test(/^watchdog_tests::/)`.

/// `retry_transient` ACs against a real wiremock completion endpoint and a
/// real `CancellationToken` — no mocks of internal types.
mod retry {
  use std::time::Duration;

  use tokio_util::sync::CancellationToken;
  use wiremock::matchers::{method, path};
  use wiremock::{Mock, MockServer, ResponseTemplate};

  use shared::RunnerError;
  use wire::reporting::ReportConclusion;
  use wire::reporting::run_service::{CompleteJobRequest, complete_job};

  use crate::retry::{REPORT_RETRY_MAX, retry_transient};

  /// Boxed error alias for helpers that use `?` — `clippy::expect_used` /
  /// `unwrap_used` are only allowed inside `#[tokio::test]` fns themselves
  /// (`clippy.toml: allow-expect-in-tests`), not their helpers.
  type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

  fn complete_request() -> CompleteJobRequest {
    CompleteJobRequest {
      plan_id: "plan-1".to_owned(),
      job_id: "job-1".to_owned(),
      request_id: 1,
      conclusion: ReportConclusion::Success,
      outputs: serde_json::Value::Object(serde_json::Map::new()),
      step_results: Vec::new(),
      annotations: Vec::new(),
    }
  }

  async fn received_count(server: &MockServer) -> TestResult<usize> {
    let requests = server
      .received_requests()
      .await
      .ok_or("request recording was disabled")?;
    Ok(requests.len())
  }

  /// AC-3: the completion endpoint fails twice (500) then succeeds — the
  /// retry survives the simulated reconnect and the mock sees all 3.
  #[tokio::test]
  async fn ac3_retries_past_transient_failures_then_succeeds() -> TestResult<()> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
      .and(path("/completejob"))
      .respond_with(ResponseTemplate::new(500))
      .up_to_n_times(2)
      .mount(&server)
      .await;
    Mock::given(method("POST"))
      .and(path("/completejob"))
      .respond_with(ResponseTemplate::new(200))
      .mount(&server)
      .await;

    let client = reqwest::Client::new();
    let url = server.uri();
    let request = complete_request();
    let cancel = CancellationToken::new();

    retry_transient(
      || async { complete_job(&client, &url, "t", &request).await },
      &cancel,
      REPORT_RETRY_MAX,
      "complete_job",
    )
    .await?;

    assert_eq!(
      received_count(&server).await?,
      3,
      "expected 2 failed attempts + 1 successful attempt"
    );
    Ok(())
  }

  /// AC-4: a definitive 400 is not retried — exactly one request reaches
  /// the mock, and the `Protocol` error propagates unwrapped.
  #[tokio::test]
  async fn ac4_definitive_error_is_not_retried() -> TestResult<()> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
      .and(path("/completejob"))
      .respond_with(ResponseTemplate::new(400))
      .mount(&server)
      .await;

    let client = reqwest::Client::new();
    let url = server.uri();
    let request = complete_request();
    let cancel = CancellationToken::new();

    let result = retry_transient(
      || async { complete_job(&client, &url, "t", &request).await },
      &cancel,
      REPORT_RETRY_MAX,
      "complete_job",
    )
    .await;

    match result {
      Err(RunnerError::Protocol(_)) => {},
      other => return Err(format!("expected Err(Protocol), got {other:?}").into()),
    }
    assert_eq!(
      received_count(&server).await?,
      1,
      "a definitive error must not be retried"
    );
    Ok(())
  }

  /// AC-7: cancelling the token mid-backoff returns promptly instead of
  /// blocking shutdown on the retry loop.
  #[tokio::test]
  async fn ac7_cancel_mid_backoff_returns_promptly() -> TestResult<()> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
      .and(path("/completejob"))
      .respond_with(ResponseTemplate::new(503))
      .mount(&server)
      .await;

    let client = reqwest::Client::new();
    let url = server.uri();
    let request = complete_request();
    let cancel = CancellationToken::new();

    let canceller = cancel.clone();
    tokio::spawn(async move {
      tokio::time::sleep(Duration::from_millis(50)).await;
      canceller.cancel();
    });

    let outcome = tokio::time::timeout(
      Duration::from_secs(2),
      retry_transient(
        || async { complete_job(&client, &url, "t", &request).await },
        &cancel,
        REPORT_RETRY_MAX,
        "complete_job",
      ),
    )
    .await
    .map_err(|_elapsed| "retry_transient did not return promptly after cancellation")?;

    match outcome {
      Err(RunnerError::Network(_)) => Ok(()),
      other => Err(format!("expected Err(Network) on cancel, got {other:?}").into()),
    }
  }

  /// AC-11: a millisecond retry budget against an endpoint that never stops
  /// returning 503 terminates with `Err(Network)` instead of spinning
  /// forever.
  #[tokio::test]
  async fn ac11_budget_exhaustion_terminates() -> TestResult<()> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
      .and(path("/completejob"))
      .respond_with(ResponseTemplate::new(503))
      .mount(&server)
      .await;

    let client = reqwest::Client::new();
    let url = server.uri();
    let request = complete_request();
    let cancel = CancellationToken::new();

    let outcome = tokio::time::timeout(
      Duration::from_secs(5),
      retry_transient(
        || async { complete_job(&client, &url, "t", &request).await },
        &cancel,
        Duration::from_millis(50),
        "complete_job",
      ),
    )
    .await
    .map_err(|_elapsed| "retry_transient did not terminate within its own retry budget")?;

    match outcome {
      Err(RunnerError::Network(_)) => Ok(()),
      other => Err(format!("expected Err(Network) on budget exhaustion, got {other:?}").into()),
    }
  }
}

/// End-to-end fixture driving the REAL `job_lifecycle::poll_and_execute`
/// against one wiremock `MockServer` (AC-2, AC-9, AC-10). Covers the whole
/// broker + Run Service exchange for one job; see
/// `watchdog_tests/trip.rs` for the fixture and the doc comment there for
/// why it lives in its own file. Filtered by the s6 ledger check
/// `test(/^watchdog_tests::/)`.
mod trip;

/// Pure unit tests for `execution_loop::apply_outage_override` — the
/// non-network fold of the watchdog's trip flag into the job's final
/// conclusion (no wiremock: plain values in, plain values out). Filtered
/// by the s6 ledger check `test(/^watchdog_tests::/)`.
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
    let (conclusion, annotations) = apply_outage_override(Conclusion::Cancelled, true);
    assert_eq!(conclusion, Conclusion::Failure);
    assert_eq!(annotations.len(), 1);
    let annotation = annotations.first().expect("checked len() == 1 above");
    assert_eq!(annotation.annotation_type, "error");
    assert_eq!(annotation.message, LOST_CONNECTION_MESSAGE);
  }
}
