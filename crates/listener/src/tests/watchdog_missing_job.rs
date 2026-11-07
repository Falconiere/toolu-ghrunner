//! Focused regression for a renewal or spool failure.

use super::*;

#[tokio::test]
async fn missing_job_cancels_immediately() -> TestResult<()> {
  let outcome = tokio::time::timeout(
    Duration::from_secs(25),
    run_scenario(Scenario::new(
      "job-missing",
      "sleep 30",
      404,
      Duration::from_secs(10),
      Duration::from_millis(100),
    )),
  )
  .await
  .map_err(|error| format!("missing job scenario timed out: {error}"))??;
  assert_reported(outcome.result, Conclusion::Failure)?;
  assert_elapsed_under(
    outcome.elapsed,
    Duration::from_secs(5),
    "missing job cancellation",
  )?;
  assert_request_count(&outcome.complete_requests, 1, "completejob")?;
  let body = outcome
    .complete_requests
    .first()
    .ok_or("missing completion")?
    .body_json::<serde_json::Value>()?;
  assert_eq!(body_conclusion(&body)?, "failed");
  let annotations = body
    .get("annotations")
    .and_then(serde_json::Value::as_array)
    .ok_or("missing annotations")?;
  let messages: Vec<_> = annotations
    .iter()
    .filter_map(|a| a.get("message").and_then(serde_json::Value::as_str))
    .collect();
  assert!(!messages.is_empty(), "missing failure explanation: {body}");
  assert!(
    messages
      .iter()
      .any(|message| message.contains("no longer recognizes this job")),
    "missing job-not-found explanation: {body}"
  );
  assert!(
    messages
      .iter()
      .all(|message| !message.to_ascii_lowercase().contains("5 minutes")),
    "404 falsely reports a five-minute outage: {body}"
  );
  Ok(())
}
