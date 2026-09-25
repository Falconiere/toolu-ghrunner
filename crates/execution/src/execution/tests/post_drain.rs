//! Elapsed-time checks for the shared post cancellation deadline.

use std::time::Duration;
use tokio::time::Instant;

use super::cancel_remaining;

#[tokio::test]
async fn cancel_budget_is_shared() {
  let start = Instant::now();
  let deadline = start + Duration::from_millis(120);
  let first = cancel_remaining(deadline);
  tokio::time::sleep(Duration::from_millis(160)).await;
  let second = cancel_remaining(deadline);
  assert!(first <= Duration::from_millis(120));
  assert_eq!(second, Duration::ZERO);
}
