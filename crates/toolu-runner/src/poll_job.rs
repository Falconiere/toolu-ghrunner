//! One claimed toolu.sh job (#206): run its JIT config once, in memory, the
//! way `boot` does, while a side task tells toolu.sh the listener started,
//! renews the claim's lease and enforces its deadline; then report how it
//! ended (`complete`).
//!
//! The job is cancelled on SIGINT/SIGTERM (the loop's cancel token), on the
//! claim's deadline (an idle JIT whose job GitHub gave another runner), and on
//! a 401 from toolu.sh mid-job (a revoked machine: nothing more is sent).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use config::config::RunnerRegistrationConfig;
use listener::GitHubListener;
use listener::session_conflict::SessionConflictPolicy;
use protocol::toolu_jobs::{Claim, CompleteBody, Outcome, heartbeat_delay};
use shared::{Conclusion, RunnerError, SecretMasker};
use tokio_util::sync::CancellationToken;
use wire::net::toolu_jobs::{TooluClient, TooluError};

use crate::boot_cmd::now_epoch_ms;
use crate::poll_loop::revoked;

/// `complete` attempts on a transient failure (1 s, 2 s, 4 s, 8 s apart).
const COMPLETE_ATTEMPTS: u32 = 5;

/// The listener's outcome, or why toolu.sh revoked the machine mid-job.
type Lifecycle = Result<Result<Option<Conclusion>, RunnerError>, String>;

/// What running one claim needs from the poll loop's pass.
pub(crate) struct ClaimedJob<'a> {
  /// The `/runner/*` client.
  pub(crate) toolu: &'a TooluClient,
  /// The pass's HTTP client, handed to the listener.
  pub(crate) client: &'a reqwest::Client,
  /// The registration (its `[services]`/`[cache]`/`[workspace]`/... apply).
  pub(crate) cfg: &'a RunnerRegistrationConfig,
  /// The process-wide secret masker.
  pub(crate) masker: &'a Arc<Mutex<SecretMasker>>,
  /// The loop's cancel token (SIGINT/SIGTERM).
  pub(crate) cancel: &'a CancellationToken,
  /// Session-conflict retry schedule of the listener.
  pub(crate) conflict_policy: SessionConflictPolicy,
}

impl ClaimedJob<'_> {
  /// Run claim `claim` of job `id` and report it; the lifecycle's outcome.
  ///
  /// # Errors
  ///
  /// The fatal [`revoked`] error when toolu.sh answers 401 (the job was
  /// cancelled first and nothing is reported).
  pub(crate) async fn run(
    &self,
    id: &str,
    claim: Claim,
  ) -> Result<Result<Option<Conclusion>, RunnerError>, Box<dyn std::error::Error>> {
    self
      .masker
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .add_secret(&claim.jit_config);
    tracing::info!(job = %id, claim_epoch = claim.claim_epoch, "claimed a toolu.sh job");
    let outcome = if now_epoch_ms() >= claim.deadline {
      Err(RunnerError::Config(
        "the claim's deadline passed before the job started".to_owned(),
      ))
    } else {
      match self.lifecycle(id, &claim).await {
        Ok(outcome) => outcome,
        Err(why) => return Err(revoked(&why)),
      }
    };
    match self.complete(id, claim.claim_epoch, &outcome).await {
      Ok(()) => Ok(outcome),
      Err(why) => Err(revoked(&why)),
    }
  }

  /// One JIT lifecycle beside the claim watcher ([`watch_claim`]).
  async fn lifecycle(&self, id: &str, claim: &Claim) -> Lifecycle {
    let runner_cfg = match crate::run_cmd::build_runner_config(self.cfg) {
      Ok(runner_cfg) => runner_cfg,
      Err(e) => return Ok(Err(RunnerError::Config(e.to_string()))),
    };
    let listener = match GitHubListener::new(
      &claim.jit_config,
      runner_cfg,
      Arc::clone(self.masker),
      self.client.clone(),
    ) {
      Ok(listener) => listener.with_session_conflict_policy(self.conflict_policy),
      Err(e) => return Ok(Err(e)),
    };
    let job_cancel = self.cancel.child_token();
    let stop = CancellationToken::new();
    let watcher = tokio::spawn(watch_claim(Watch {
      toolu: self.toolu.clone(),
      id: id.to_owned(),
      claim_epoch: claim.claim_epoch,
      lease_expires_at: claim.lease_expires_at,
      deadline: claim.deadline,
      job_cancel: job_cancel.clone(),
      stop: stop.clone(),
    }));
    let outcome = listener.run(job_cancel).await;
    stop.cancel();
    match watcher.await {
      Ok(Some(why)) => Err(why),
      Ok(None) => Ok(outcome),
      Err(e) => {
        tracing::error!(error = %e, "the toolu.sh claim watcher failed");
        Ok(outcome)
      },
    }
  }

  /// `complete` the claim: `done` when a job ran to a conclusion, else
  /// `failed` with the masked error. Transient failures are retried
  /// [`COMPLETE_ATTEMPTS`] times regardless of cancellation (a SIGTERM'd job
  /// is still reported); a lost claim or a permanent failure is logged.
  /// `Err` carries a 401's reason.
  async fn complete(
    &self,
    id: &str,
    claim_epoch: u64,
    outcome: &Result<Option<Conclusion>, RunnerError>,
  ) -> Result<(), String> {
    let body = complete_body(outcome, claim_epoch, self.masker);
    let mut delay = Duration::from_secs(1);
    for attempt in 1..=COMPLETE_ATTEMPTS {
      match self.toolu.complete(id, &body).await {
        Ok(()) => {
          tracing::info!(job = %id, outcome = ?body.outcome, "reported the job to toolu.sh");
          return Ok(());
        },
        Err(TooluError::Revoked(why)) => return Err(why),
        Err(e @ (TooluError::Conflict(_) | TooluError::NotFound | TooluError::Protocol(_))) => {
          tracing::warn!(job = %id, error = %e, "toolu.sh did not take the report; moving on");
          return Ok(());
        },
        Err(e @ TooluError::Transient(_)) if attempt < COMPLETE_ATTEMPTS => {
          tracing::warn!(job = %id, error = %e, attempt, "reporting the job failed; retrying");
          tokio::time::sleep(delay).await;
          delay = delay.saturating_mul(2);
        },
        Err(e @ TooluError::Transient(_)) => {
          tracing::warn!(
            job = %id,
            error = %e,
            "giving up reporting the job; toolu.sh reclaims it when the lease lapses"
          );
        },
      }
    }
    Ok(())
  }
}

/// The `complete` body for `outcome`.
fn complete_body(
  outcome: &Result<Option<Conclusion>, RunnerError>,
  claim_epoch: u64,
  masker: &Mutex<SecretMasker>,
) -> CompleteBody {
  let (outcome, error_message) = match outcome {
    Ok(Some(_conclusion)) => (Outcome::Done, None),
    Ok(None) => (
      Outcome::Failed,
      Some("no job ran: the lifecycle was cancelled before GitHub assigned one".to_owned()),
    ),
    Err(e) => (
      Outcome::Failed,
      Some(crate::app_auth::masked(masker, &e.to_string())),
    ),
  };
  CompleteBody {
    outcome,
    error_message,
    claim_epoch,
  }
}

/// What the claim watcher owns.
struct Watch {
  toolu: TooluClient,
  id: String,
  claim_epoch: u64,
  lease_expires_at: u64,
  deadline: u64,
  /// Cancels the running job (deadline, revocation).
  job_cancel: CancellationToken,
  /// The lifecycle ended: stop at once.
  stop: CancellationToken,
}

/// Post `dispatched`, then renew the lease at [`heartbeat_delay`] and cancel
/// the job at the claim's deadline, until `stop`. A 409/404 heartbeat stops
/// renewing (the claim is lost; the job runs to its end), a transient one is
/// retried. `Some(reason)` when toolu.sh answered 401: the job was cancelled.
async fn watch_claim(w: Watch) -> Option<String> {
  if let Some(end) = mark_dispatched(&w).await {
    return end;
  }
  let deadline = tokio::time::sleep(Duration::from_millis(
    w.deadline.saturating_sub(now_epoch_ms()),
  ));
  tokio::pin!(deadline);
  let (mut lease, mut renewing, mut deadline_fired) = (w.lease_expires_at, true, false);
  loop {
    let next = heartbeat_delay(lease, now_epoch_ms());
    tokio::select! {
      () = w.stop.cancelled() => return None,
      () = &mut deadline, if !deadline_fired => {
        deadline_fired = true;
        tracing::warn!(job = %w.id, "the claim's deadline passed; cancelling the job");
        w.job_cancel.cancel();
      },
      () = tokio::time::sleep(next), if renewing => {
        let renewal = tokio::select! {
          () = w.stop.cancelled() => return None,
          result = w.toolu.heartbeat(&w.id, w.claim_epoch) => result,
        };
        match renewal {
          Ok(renewal) => lease = renewal.lease_expires_at,
          Err(TooluError::Revoked(why)) => return Some(revoke(&w, why)),
          Err(e @ (TooluError::Conflict(_) | TooluError::NotFound)) => {
            tracing::warn!(job = %w.id, error = %e, "the claim was lost; the job runs to its end");
            renewing = false;
          },
          Err(e @ (TooluError::Transient(_) | TooluError::Protocol(_))) => {
            tracing::warn!(job = %w.id, error = %e, "renewing the lease failed; retrying");
          },
        }
      },
    }
  }
}

/// Post `dispatched` (advisory: only a 401 matters). `Some` ends the watch
/// with that result: `stop` fired, or the machine was revoked.
async fn mark_dispatched(w: &Watch) -> Option<Option<String>> {
  let dispatched = tokio::select! {
    () = w.stop.cancelled() => return Some(None),
    result = w.toolu.dispatched(&w.id, w.claim_epoch) => result,
  };
  match dispatched {
    Ok(()) => tracing::info!(job = %w.id, "told toolu.sh the job's listener started"),
    Err(TooluError::Revoked(why)) => return Some(Some(revoke(w, why))),
    Err(
      e @ (TooluError::Conflict(_)
      | TooluError::NotFound
      | TooluError::Transient(_)
      | TooluError::Protocol(_)),
    ) => {
      tracing::warn!(job = %w.id, error = %e, "could not mark the claim dispatched; running anyway");
    },
  }
  None
}

/// A 401 mid-job: cancel the job and hand back the reason.
fn revoke(w: &Watch, why: String) -> String {
  tracing::error!(job = %w.id, "toolu.sh revoked this machine mid-job; cancelling the job");
  w.job_cancel.cancel();
  why
}
