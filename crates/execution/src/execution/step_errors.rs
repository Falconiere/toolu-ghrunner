//! Reports a hard top-level step error before its timeline completion.

use std::collections::HashMap;

use shared::{Conclusion, LogStream, RunnerError, RunnerEvent};
use tokio::sync::mpsc;

/// Emit the parent error line before closing its step record.
pub(super) async fn report_step_failure(
  events: &mpsc::Sender<RunnerEvent>,
  step_id: &str,
  err: &RunnerError,
) {
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
      "event channel closed; step-failure log line was dropped"
    );
  }
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
      "event channel closed; step-failure completion event was dropped"
    );
  }
}
