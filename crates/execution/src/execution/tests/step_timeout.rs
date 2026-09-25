//! A blocked resolution operation must obey the step's absolute deadline.

use std::time::Duration;

use shared::RunnerError;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::StepBounds;

#[tokio::test]
async fn blocked_resolution_obeys_parent_deadline() -> Result<(), Box<dyn std::error::Error>> {
  let (events, _receiver) = mpsc::channel(1);
  events.send("occupied").await?;
  let bounds = StepBounds {
    timeout: Some(Duration::from_millis(30)),
    deadline: Some(Instant::now() + Duration::from_millis(30)),
    cancel: CancellationToken::new(),
  };
  let pending_send = async {
    events
      .send("resolution log")
      .await
      .map_err(|error| RunnerError::StepExecution(error.to_string()))?;
    Ok(())
  };
  let result = bounds.resolve_within_bounds(pending_send).await;
  assert!(
    matches!(result, Err(RunnerError::StepExecution(message)) if message.contains("timed out while resolving action"))
  );
  Ok(())
}

#[tokio::test]
async fn blocked_resolution_obeys_cancellation() -> Result<(), Box<dyn std::error::Error>> {
  let (events, _receiver) = mpsc::channel(1);
  events.send("occupied").await?;
  let cancel = CancellationToken::new();
  let bounds = StepBounds {
    timeout: None,
    deadline: None,
    cancel: cancel.clone(),
  };
  let pending_send = async {
    events
      .send("resolution log")
      .await
      .map_err(|error| RunnerError::StepExecution(error.to_string()))?;
    Ok(())
  };
  cancel.cancel();
  let result = bounds.resolve_within_bounds(pending_send).await;
  assert!(matches!(result, Err(RunnerError::Cancelled)));
  Ok(())
}
