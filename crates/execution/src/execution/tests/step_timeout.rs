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
  let bounds = StepBounds::nested(
    Some(Instant::now() + Duration::from_millis(30)),
    None,
    CancellationToken::new(),
  );
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
  let bounds = StepBounds::nested(None, None, cancel.clone());
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

/// Upstream gives an action's `pre` and `main` separate `timeout-minutes`
/// budgets: a restarted bound re-arms the own timeout from now.
#[tokio::test]
async fn restarted_bounds_rearm_the_own_timeout() {
  let first = StepBounds::nested(
    None,
    Some(Duration::from_secs(60)),
    CancellationToken::new(),
  );
  tokio::time::sleep(Duration::from_millis(50)).await;
  let second = first.restarted();
  let gap = second
    .deadline
    .zip(first.deadline)
    .map(|(second, first)| second - first);
  assert!(
    gap.is_some_and(|gap| gap >= Duration::from_millis(50)),
    "{gap:?}"
  );
  assert!(
    second
      .remaining_timeout()
      .is_some_and(|left| left > Duration::from_secs(59))
  );
}

/// Composite children carry no own timeout, so restarting keeps the
/// enclosing step's single remaining budget; an own timeout never outlives it.
#[tokio::test]
async fn restarted_bounds_keep_the_enclosing_deadline() {
  let parent = Instant::now() + Duration::from_secs(60);
  let first = StepBounds::nested(Some(parent), None, CancellationToken::new());
  tokio::time::sleep(Duration::from_millis(20)).await;
  assert_eq!(first.restarted().deadline, Some(parent));
  let capped = StepBounds::nested(
    Some(parent),
    Some(Duration::from_secs(60)),
    CancellationToken::new(),
  );
  assert_eq!(capped.restarted().deadline, Some(parent));
}
