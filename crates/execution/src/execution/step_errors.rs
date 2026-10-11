//! Reports a hard job-step error, including nested failures attributed to its parent.

use std::collections::HashMap;

use shared::{Conclusion, LogStream, RunnerError, RunnerEvent};
use tokio::sync::mpsc;

/// Emit the parent error line before closing its step record.
pub(super) async fn report_step_failure(
  events: &mpsc::Sender<RunnerEvent>,
  step_id: &str,
  err: &RunnerError,
) {
  report_step_error(events, step_id, err).await;
  if events
    .send(RunnerEvent::StepCompleted {
      step_id: step_id.to_owned(),
      conclusion: Conclusion::Failure,
      outputs: HashMap::new(),
    })
    .await
    .is_err()
  {
    tracing::warn!(
      step_id,
      error = %err,
      "event channel closed; step-failure completion event was dropped"
    );
  }
}

/// Emit an execution error without completing the step before result adjustment.
pub(super) async fn report_step_error(
  events: &mpsc::Sender<RunnerEvent>,
  step_id: &str,
  err: &RunnerError,
) {
  report_infrastructure_error(events, step_id, err).await;
  if events
    .send(RunnerEvent::Log {
      step_id: step_id.to_owned(),
      line: format!("##[error]{err}"),
      stream: LogStream::Stdout,
    })
    .await
    .is_err()
  {
    tracing::warn!(
      step_id,
      error = %err,
      "event channel closed; step-failure log line was dropped"
    );
  }
}

/// Attribute a categorized runner-infrastructure failure (an action fetch the
/// service or network failed) to the reported step that hit it.
pub(crate) async fn report_infrastructure_error(
  events: &mpsc::Sender<RunnerEvent>,
  step_id: &str,
  err: &RunnerError,
) {
  let RunnerError::ActionFetch(fetch) = err else {
    return;
  };
  let Some(category) = fetch.infrastructure_category() else {
    return;
  };
  let event = RunnerEvent::InfrastructureError {
    step_id: step_id.to_owned(),
    category: category.to_owned(),
    message: fetch.message.clone(),
  };
  if events.send(event).await.is_err() {
    tracing::warn!(
      step_id,
      "event channel closed; infrastructure error was dropped"
    );
  }
}
