//! Pure per-iteration decision for the always-online run loop.
//!
//! Separated from the bin crate's loop driver so the "what does the loop do
//! after a listener lifecycle returns" decision can be unit-tested without a
//! live broker, a token store, or the filesystem. Mirrors `message_route` in
//! spirit: one pure function over already-observed facts.

use shared::{Conclusion, RunnerError, SESSION_CONFLICT_EXIT_CODE};

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
/// 2. `once` → `Exit(0)` on `Ok`, `Exit(SESSION_CONFLICT_EXIT_CODE)` on
///    `Err(SessionConflict(_))`, `Exit(1)` on any other `Err` (legacy
///    single-job).
/// 3. `pending_remove` → `Exit(0)` (a `remove` ran while the lock was held).
/// 4. `Err(SessionConflict(_))` → `Exit(SESSION_CONFLICT_EXIT_CODE)` — the
///    listener already spent its bounded retry; the loop must not extend it.
/// 5. `Ok(())` → `Reregister` (JIT config spent; mint a fresh one).
/// 6. `Err(Auth(_))` → `Reregister` (JIT config dead; retry can't succeed).
/// 7. Any other `Err` → `BackoffRetry` (transient; same JIT config still valid).
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
      Err(RunnerError::SessionConflict(_)) => LoopAction::Exit(SESSION_CONFLICT_EXIT_CODE),
      Err(_) => LoopAction::Exit(1),
    };
  }
  if pending_remove {
    return LoopAction::Exit(0);
  }
  match outcome {
    Err(RunnerError::SessionConflict(_)) => LoopAction::Exit(SESSION_CONFLICT_EXIT_CODE),
    // Rule 5 (job done, JIT spent) and rule 6 (auth-dead JIT) both recover by
    // minting a fresh config; only rule 7's transient errors reuse the config.
    Ok(()) | Err(RunnerError::Auth(_)) => LoopAction::Reregister,
    Err(_) => LoopAction::BackoffRetry,
  }
}

/// Facts the run loop observed after a lifecycle, besides its outcome.
#[derive(Debug, Clone, Copy)]
pub struct LoopSignals {
  /// `run --once` was passed.
  pub once: bool,
  /// SIGINT/SIGTERM fired (graceful shutdown).
  pub cancelled: bool,
  /// A `remove` wrote `.pending_remove` while this run held the lock.
  pub pending_remove: bool,
}

/// What `run` does after a listener lifecycle on an **ephemeral**
/// registration (`register --ephemeral`): it never re-mints.
#[derive(Debug, PartialEq, Eq)]
pub enum EphemeralAction {
  /// A job ran and consumed the registration: unregister it and delete the
  /// local state, then exit.
  Unregister,
  /// Terminate with this process exit code, keeping the registration.
  Exit(i32),
  /// Sleep with cancel-aware jittered backoff, then retry the same, still
  /// unused JIT config.
  BackoffRetry,
}

/// Decide an ephemeral registration's next action (#92). First match wins:
///
/// 1. a job ran (`Ok(Some(_))`, any conclusion, even after a cancel) or was
///    acquired before the lifecycle errored (`job_acquired`: execution or
///    completion failed, but the job consumed the JIT) → `Unregister`;
/// 2. `signals.cancelled` → `Exit(0)` (shut down before a job; registration kept);
/// 3. `signals.once` → `Exit(0)` on `Ok(None)`, `Exit(SESSION_CONFLICT_EXIT_CODE)` on a
///    session conflict, `Exit(1)` on any other `Err`;
/// 4. `signals.pending_remove` → `Exit(0)`;
/// 5. `Err(SessionConflict(_))` → `Exit(SESSION_CONFLICT_EXIT_CODE)`;
/// 6. `Err(Auth(_))` → `Exit(1)` (the JIT is dead and is never re-minted);
/// 7. `Ok(None)` or any other `Err` → `BackoffRetry`.
pub fn next_ephemeral_action(
  outcome: &Result<Option<Conclusion>, RunnerError>,
  job_acquired: bool,
  signals: LoopSignals,
) -> EphemeralAction {
  if job_acquired || matches!(outcome, Ok(Some(_))) {
    return EphemeralAction::Unregister;
  }
  if signals.cancelled {
    return EphemeralAction::Exit(0);
  }
  if signals.once {
    return match outcome {
      Ok(_) => EphemeralAction::Exit(0),
      Err(RunnerError::SessionConflict(_)) => EphemeralAction::Exit(SESSION_CONFLICT_EXIT_CODE),
      Err(_) => EphemeralAction::Exit(1),
    };
  }
  if signals.pending_remove {
    return EphemeralAction::Exit(0);
  }
  match outcome {
    Err(RunnerError::SessionConflict(_)) => EphemeralAction::Exit(SESSION_CONFLICT_EXIT_CODE),
    Err(RunnerError::Auth(_)) => EphemeralAction::Exit(1),
    _ => EphemeralAction::BackoffRetry,
  }
}
