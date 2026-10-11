//! Bounded retry for broker session-create conflicts (HTTP 409).
//!
//! GitHub's broker refuses a second session for a registration while one is
//! live, and keeps refusing for a few minutes after that session ends (~181 s
//! observed on github.com). Upstream retries every 30 s until 4 minutes of
//! conflicts accumulate, then exits with `ReturnCode.SessionConflict`;
//! [`SessionConflictPolicy::default`] mirrors that schedule. Only
//! `RunnerError::SessionConflict` is retried — every other error returns
//! unchanged so its own classification decides what happens next.

use std::future::Future;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use shared::RunnerError;

/// Upstream's `_sessionCreationRetryInterval`.
const DEFAULT_INTERVAL: Duration = Duration::from_secs(30);
/// Upstream stops once `conflicts × 30 s ≥ 4 min`, i.e. on the 8th conflict.
const DEFAULT_MAX_ATTEMPTS: u32 = 8;

/// How long session creation keeps retrying a conflict before giving up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionConflictPolicy {
  /// Sleep between a conflicted attempt and the next one.
  pub interval: Duration,
  /// Total session attempts (the first included) before the conflict is final.
  pub max_attempts: u32,
}

impl Default for SessionConflictPolicy {
  fn default() -> Self {
    Self {
      interval: DEFAULT_INTERVAL,
      max_attempts: DEFAULT_MAX_ATTEMPTS,
    }
  }
}

/// Run `op` until it stops returning `SessionConflict` or the policy is spent.
///
/// `Ok(Some(v))` on success; `Ok(None)` when `cancel` fires while waiting to
/// retry (or is already set when a conflict arrives) — a deliberate shutdown,
/// not a failure; the last `SessionConflict` once `max_attempts` attempts all
/// conflicted; any other error from `op` unchanged.
pub(crate) async fn retry_session_conflict<T, F, Fut>(
  mut op: F,
  cancel: &CancellationToken,
  policy: SessionConflictPolicy,
) -> Result<Option<T>, RunnerError>
where
  F: FnMut() -> Fut,
  Fut: Future<Output = Result<T, RunnerError>>,
{
  let max_attempts = policy.max_attempts.max(1);
  let mut attempt: u32 = 1;
  loop {
    match op().await {
      Ok(value) => return Ok(Some(value)),
      Err(RunnerError::SessionConflict(msg)) if attempt >= max_attempts => {
        log_spent(attempt);
        return Err(RunnerError::SessionConflict(msg));
      },
      Err(RunnerError::SessionConflict(_)) => {
        log_retry(attempt, max_attempts, policy.interval);
        if cancelled_while_waiting(cancel, policy.interval).await {
          return Ok(None);
        }
        attempt += 1;
      },
      Err(other) => return Err(other),
    }
  }
}

/// Sleep `interval`, waking early on `cancel`; `true` when cancel won.
async fn cancelled_while_waiting(cancel: &CancellationToken, interval: Duration) -> bool {
  tokio::select! {
    () = cancel.cancelled() => {
      tracing::info!("cancelled while waiting to retry a session conflict");
      true
    },
    () = tokio::time::sleep(interval) => false,
  }
}

fn log_retry(attempt: u32, max_attempts: u32, interval: Duration) {
  tracing::warn!(
    "session conflict on attempt {attempt}/{max_attempts}: another runner session is active for \
     this registration; retrying in {}s",
    interval.as_secs_f64()
  );
}

fn log_spent(attempts: u32) {
  tracing::error!(
    attempts,
    "session conflict persisted for every attempt; another runner session is active for this \
     registration — giving up"
  );
}
