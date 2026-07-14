//! Pure per-iteration decision for the always-online run loop.
//!
//! Separated from the bin crate's loop driver so the "what does the loop do
//! after a listener lifecycle returns" decision can be unit-tested without a
//! live broker, a token store, or the filesystem. Mirrors `message_route` in
//! spirit: one pure function over already-observed facts.

use shared::RunnerError;

/// What the run loop does after a listener lifecycle returns.
#[derive(Debug, PartialEq, Eq)]
pub enum LoopAction {
  /// Terminate the loop with this process exit code.
  Exit(i32),
  /// Re-mint a fresh JIT config and build a new listener (the current JIT
  /// config is consumed).
  Reregister,
  /// Sleep with cancel-aware jittered backoff, then retry the same JIT config.
  BackoffRetry,
}

/// Decide the loop's next action from the facts observed after a lifecycle.
///
/// Normative order, first match wins (`cancelled` is the only signal that
/// tells graceful shutdown from a completed job — both return `Ok`):
///
/// 1. `cancelled` → `Exit(0)` (graceful shutdown).
/// 2. `once` → `Exit(0)` on `Ok`, `Exit(1)` on `Err` (legacy single-job).
/// 3. `pending_remove` → `Exit(0)` (a `remove` ran while the lock was held).
/// 4. `Ok(())` → `Reregister` (JIT config spent; mint a fresh one).
/// 5. `Err(Auth(_))` → `Reregister` (JIT config dead; retry can't succeed).
/// 6. Any other `Err` → `BackoffRetry` (transient; same JIT config still valid).
pub fn next_action(
  outcome: &Result<(), RunnerError>,
  once: bool,
  cancelled: bool,
  pending_remove: bool,
) -> LoopAction {
  if cancelled {
    return LoopAction::Exit(0);
  }
  if once {
    return match outcome {
      Ok(()) => LoopAction::Exit(0),
      Err(_) => LoopAction::Exit(1),
    };
  }
  if pending_remove {
    return LoopAction::Exit(0);
  }
  match outcome {
    // Rule 4 (job done, JIT spent) and rule 5 (auth-dead JIT) both recover by
    // minting a fresh config; only rule 6's transient errors reuse the config.
    Ok(()) | Err(RunnerError::Auth(_)) => LoopAction::Reregister,
    Err(_) => LoopAction::BackoffRetry,
  }
}
