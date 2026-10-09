//! A blocked resolution operation must obey the step's absolute deadline.

use std::time::Duration;

use shared::RunnerError;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::{
  NESTED_TIMEOUT_MESSAGE, StepBounds, step_timeout_message, timeout_message, with_timeout_message,
};

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

/// Upstream's `StepsRunner` names the step and its evaluated whole minutes.
#[test]
fn top_level_message_names_the_step_and_its_minutes() {
  assert_eq!(
    step_timeout_message("Deferred timeout", Some(Duration::from_secs(60))),
    "The action 'Deferred timeout' has timed out after 1 minutes."
  );
  assert_eq!(
    step_timeout_message("Post Build", Some(Duration::from_secs(5 * 60))),
    "The action 'Post Build' has timed out after 5 minutes."
  );
  assert_eq!(
    step_timeout_message("No budget", None),
    NESTED_TIMEOUT_MESSAGE
  );
}

/// The innermost scope wins, so a composite child inside a top-level step
/// reports upstream's nested wording; outside any scope the fallback applies.
#[tokio::test]
async fn innermost_scope_supplies_the_message() {
  assert_eq!(timeout_message(), NESTED_TIMEOUT_MESSAGE);
  let top = "The action 'Composite' has timed out after 1 minutes.".to_owned();
  let (outer, inner) = with_timeout_message(top.clone(), async {
    let inner = with_timeout_message(NESTED_TIMEOUT_MESSAGE.to_owned(), async {
      timeout_message()
    })
    .await;
    (timeout_message(), inner)
  })
  .await;
  assert_eq!(outer, top);
  assert_eq!(inner, NESTED_TIMEOUT_MESSAGE);
}
