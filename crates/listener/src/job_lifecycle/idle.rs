//! The idle half of the job lifecycle: long-poll the broker until a job
//! message arrives, the session ends, or a drain asks this runner to stop
//! taking jobs.
//!
//! A drain (#208) is how `run --all` stops one registration's loop for a
//! pending removal without touching an in-flight job: it is honored only
//! here, before a job is acquired. Once [`poll_until_job`] hands a job
//! message back, the job runs to completion whatever the drain token says.

use std::future::Future;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use super::{
  POLL_BACKOFF_MAX, POLL_BACKOFF_START, backoff_after_poll_error, is_redelivery, poll_once,
  sleep_or_cancel,
};
use crate::SessionCtx;
use crate::broker_message::{PollOutcome, log_skipped_control};
use crate::broker_refresh::refresh_access_token;
use protocol::messages::BrokerMessage;
use shared::RunnerError;

/// Poll until a job message arrives (`Some`), or until the lifecycle should
/// end without one (`None`): cancellation, a broker shutdown or
/// cancellation message, or `drain` firing while idle.
pub(super) async fn poll_until_job(
  ctx: &mut SessionCtx,
  drain: &CancellationToken,
) -> Result<Option<BrokerMessage>, RunnerError> {
  let mut backoff = POLL_BACKOFF_START;
  // Redelivery cursor: `0` until the first message, then the id of the last
  // message handled. Sent on every poll so the broker skips re-served ones.
  let mut last_message_id: i64 = 0;
  loop {
    // Checked every iteration as well as raced below: a step that completes
    // immediately (a classified `NoWork`) must not let a drain go unseen.
    if drain.is_cancelled() {
      return Ok(drained());
    }
    let Some(result) = unless_drained(drain, poll_once(ctx, &ctx.token, last_message_id)).await
    else {
      return Ok(drained());
    };
    if let Some(id) = result.message_id() {
      if is_redelivery(last_message_id, &result) {
        tracing::warn!(
          message_id = id,
          "broker redelivered an already-seen message"
        );
        match unless_drained(drain, sleep_or_cancel(ctx, backoff)).await {
          None => return Ok(drained()),
          Some(true) => return Ok(None),
          Some(false) => {},
        }
        backoff = backoff.saturating_mul(2).min(POLL_BACKOFF_MAX);
        continue;
      }
      last_message_id = id;
    }
    let Some(decision) =
      unless_drained(drain, handle_idle_outcome(ctx, result, &mut backoff)).await
    else {
      return Ok(drained());
    };
    match decision? {
      IdleDecision::Continue => {},
      IdleDecision::Exit => return Ok(None),
      IdleDecision::Job(msg) => return Ok(Some(msg)),
    }
  }
}

/// Run `fut` unless `drain` fires while it is pending (`None`). `biased`
/// polls `fut` first on every wake-up, so `fut` wins whenever it is ready —
/// a job message that arrives together with the drain is still taken, and a
/// `fut` that is ready on its first poll never observes a drain that fired
/// earlier; [`poll_until_job`] checks the drain at the top of each iteration
/// for that case.
async fn unless_drained<T>(drain: &CancellationToken, fut: impl Future<Output = T>) -> Option<T> {
  tokio::select! {
    biased;
    value = fut => Some(value),
    () = drain.cancelled() => None,
  }
}

/// Log the drain and end the idle loop with no job.
fn drained() -> Option<BrokerMessage> {
  tracing::info!("drain requested while idle — no longer taking jobs");
  None
}

/// Effect of one broker outcome on the idle poll loop.
pub(super) enum IdleDecision {
  Continue,
  Exit,
  Job(BrokerMessage),
}

/// Apply a classified broker outcome after its cursor has been advanced.
pub(super) async fn handle_idle_outcome(
  ctx: &mut SessionCtx,
  outcome: PollOutcome,
  backoff: &mut Duration,
) -> Result<IdleDecision, RunnerError> {
  match outcome {
    PollOutcome::Cancelled => return Ok(IdleDecision::Exit),
    PollOutcome::NoWork => {},
    PollOutcome::Migrated { url, .. } => ctx.broker_url = url,
    PollOutcome::Job(msg) => return Ok(IdleDecision::Job(msg)),
    PollOutcome::Cancel { msg: _, job_id } => {
      handle_cancellation(ctx, &job_id);
      return Ok(IdleDecision::Exit);
    },
    PollOutcome::NetworkError(e) => {
      return Ok(match backoff_after_poll_error(ctx, &e, *backoff).await {
        Some(next) => {
          *backoff = next;
          IdleDecision::Continue
        },
        None => IdleDecision::Exit,
      });
    },
    control @ (PollOutcome::Skip { .. }
    | PollOutcome::Unknown { .. }
    | PollOutcome::UnsupportedControl { .. }
    | PollOutcome::Shutdown { .. }
    | PollOutcome::RefreshToken { .. }) => {
      let decision = handle_idle_control(ctx, control).await?;
      *backoff = POLL_BACKOFF_START;
      return Ok(decision);
    },
  }
  *backoff = POLL_BACKOFF_START;
  Ok(IdleDecision::Continue)
}

/// Handle a control envelope after the idle cursor has advanced.
async fn handle_idle_control(
  ctx: &mut SessionCtx,
  outcome: PollOutcome,
) -> Result<IdleDecision, RunnerError> {
  match outcome {
    PollOutcome::Skip { .. }
    | PollOutcome::Unknown { .. }
    | PollOutcome::UnsupportedControl { .. } => log_skipped_control(&outcome),
    PollOutcome::Shutdown { message_id } => {
      tracing::info!(message_id, "broker requested runner shutdown");
      ctx.cancel.cancel();
      return Ok(IdleDecision::Exit);
    },
    PollOutcome::RefreshToken { message_id } => return refresh_idle_token(ctx, message_id).await,
    PollOutcome::NoWork
    | PollOutcome::Migrated { .. }
    | PollOutcome::Job(_)
    | PollOutcome::Cancel { .. }
    | PollOutcome::NetworkError(_)
    | PollOutcome::Cancelled => {},
  }
  Ok(IdleDecision::Continue)
}

/// Publish a replacement token only after a complete successful exchange.
async fn refresh_idle_token(
  ctx: &mut SessionCtx,
  message_id: i64,
) -> Result<IdleDecision, RunnerError> {
  match refresh_access_token(ctx).await {
    Ok(next) => {
      ctx.token = next;
      tracing::info!(message_id, "broker token refreshed");
      Ok(IdleDecision::Continue)
    },
    Err(RunnerError::Cancelled) => Ok(IdleDecision::Exit),
    Err(e) => Err(e),
  }
}

/// Cancel the in-flight job token.
///
/// The shared `ctx.cancel` token is observed by the poll loop, the renewal
/// task, and the running job, so cancelling it stops the whole pipeline.
/// No broker ack is sent: `acknowledge` validates `runnerRequestId` (the
/// job request's UUID) and a `JobCancellation` body carries only a `jobId`,
/// so an ack keyed by it is always rejected with a 400. Redelivery is
/// prevented by the poll's `lastMessageId` cursor advancing past the message.
fn handle_cancellation(ctx: &SessionCtx, job_id: &str) {
  tracing::info!(
    job_id,
    "received JobCancellation — cancelling in-flight job"
  );
  ctx.cancel.cancel();
}
