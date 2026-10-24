//! `run` for a toolu.sh machine (`[provider] kind = "toolu"`, #206): poll
//! toolu.sh's `/runner/*` queue, claim one job at a time, run its JIT config
//! once ([`crate::poll_job`]), and repeat.
//!
//! Contract: CodaSignal/toolu.sh `docs/guides/self-hosted-machines.md`. The
//! guarantees are `run`'s: the caller holds the registration's `.lock`; the
//! cancel token ends the loop (mid-job: a graceful cancel, then the job is
//! reported); 429/5xx/transport failures back off 1 s → 60 s (reset once a
//! poll finds nothing to claim or a job ran); a 401 is fatal
//! (the machine was revoked). A lost claim race just moves on. The config
//! and host token are reloaded every pass, so a re-pair or a `[network]`
//! edit applies without a restart.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use config::config::{RunnerRegistrationConfig, load_credentials, resolve_data_dir};
use config::provider::ProviderSection;
use listener::loop_decision::{LoopAction, next_action};
use listener::session_conflict::SessionConflictPolicy;
use protocol::toolu_jobs::{ClaimRefusal, parse_claim_refusal};
use shared::{Conclusion, RunnerError, SecretMasker};
use tokio_util::sync::CancellationToken;
use wire::net::toolu_jobs::{TooluClient, TooluError};

use crate::cli::credentials_path_for;
use crate::poll_job::ClaimedJob;
use crate::run_cmd::{
  self, BACKOFF_START, LOOP_CLIENT_TIMEOUT, LoopInputs, backoff_step, load_run_config,
  sleep_or_stop,
};

/// What one poll pass ended with.
enum Pass {
  /// Nothing to claim (empty queue, every job taken, slots full): wait the
  /// poll interval.
  Idle,
  /// toolu.sh did not answer usefully: back off.
  Backoff,
  /// A claimed job's JIT lifecycle ran with this outcome.
  Ran(Result<Option<Conclusion>, RunnerError>),
}

/// Loop-invariant handles of a toolu.sh machine's poll loop.
pub(crate) struct PollLoop {
  config_path: PathBuf,
  creds_path: PathBuf,
  pending_path: PathBuf,
  masker: Arc<Mutex<SecretMasker>>,
  /// Cancels the loop and any in-flight job (SIGINT/SIGTERM).
  cancel: CancellationToken,
  /// Stops the loop between jobs (`run --all` removal); never a claimed job.
  drain: CancellationToken,
  once: bool,
  conflict_policy: SessionConflictPolicy,
  poll_interval: Duration,
}

impl PollLoop {
  /// Build the loop for a toolu.sh registration whose lock the caller holds.
  ///
  /// # Errors
  ///
  /// An `ephemeral` registration (a machine is persistent; every claimed JIT
  /// is already single-use), or an unresolvable data dir.
  pub(crate) fn for_registration(
    inputs: LoopInputs<'_>,
  ) -> Result<Self, Box<dyn std::error::Error>> {
    let LoopInputs {
      config_path,
      cfg,
      masker,
      cancel,
      drain,
      once,
    } = inputs;
    if cfg.ephemeral {
      return Err(
        format!(
          "{} is a toolu.sh machine and cannot be ephemeral (every job it claims already runs \
           a single-use JIT config) — remove `ephemeral = true`",
          config_path.display()
        )
        .into(),
      );
    }
    let data_dir = resolve_data_dir(&cfg.runtime.data_dir).map_err(|e| format!("{e}"))?;
    Ok(Self {
      creds_path: credentials_path_for(&config_path),
      pending_path: data_dir.join(".pending_remove"),
      config_path,
      masker,
      cancel,
      drain,
      once,
      conflict_policy: crate::config::session_conflict_policy(),
      poll_interval: crate::config::poll_interval(),
    })
  }

  /// Poll until cancel, drain, `.pending_remove`, `--once` after a job, or a
  /// fatal error (unusable config or host token, a revoked machine).
  pub(crate) async fn drive(self) -> Result<(), Box<dyn std::error::Error>> {
    let mut backoff = BACKOFF_START;
    let mut network_memo = crate::network_setup::NetworkMemo::default();
    let mut masked_token = String::new();
    loop {
      if self.cancel.is_cancelled() || self.drain.is_cancelled() || self.pending_path.exists() {
        tracing::info!("stopping the toolu.sh poll loop between jobs");
        return Ok(());
      }
      // Bound first: a `?` temporary in the scrutinee would live across the
      // `await`s below, making the loop future `!Send` (`run --all`).
      let pass = self.pass(&mut network_memo, &mut masked_token).await?;
      if let Some(end) = self.settle(pass, &mut backoff).await {
        return end;
      }
    }
  }

  /// One pass on the freshly reloaded config: build the clients, then list
  /// and claim ([`Self::poll_once`]).
  async fn pass(
    &self,
    network_memo: &mut crate::network_setup::NetworkMemo,
    masked_token: &mut String,
  ) -> Result<Pass, Box<dyn std::error::Error>> {
    let cfg = load_run_config(&self.config_path)?;
    let client = crate::network_setup::lifecycle_client(
      &cfg.network_config(),
      &self.masker,
      LOOP_CLIENT_TIMEOUT,
      network_memo,
    )?;
    let toolu = self.toolu_client(&cfg, &client, masked_token)?;
    self.poll_once(&cfg, &client, &toolu).await
  }

  /// Wait as `pass` calls for; `Some` ends the loop with that result.
  async fn settle(
    &self,
    pass: Pass,
    backoff: &mut Duration,
  ) -> Option<Result<(), Box<dyn std::error::Error>>> {
    let back_off = match pass {
      Pass::Idle => {
        *backoff = BACKOFF_START;
        return sleep_or_stop(&self.cancel, &self.drain, self.poll_interval)
          .await
          .then_some(Ok(()));
      },
      Pass::Backoff => true,
      Pass::Ran(outcome) => {
        let outcome = outcome.map(|_conclusion| ());
        let cancelled = self.cancel.is_cancelled() || self.drain.is_cancelled();
        match next_action(&outcome, self.once, cancelled, self.pending_path.exists()) {
          LoopAction::Exit(code) => return Some(run_cmd::finish(code, outcome)),
          // A finished (or auth-dead) JIT is spent either way: poll again.
          LoopAction::Reregister => {
            *backoff = BACKOFF_START;
            false
          },
          LoopAction::BackoffRetry => true,
        }
      },
    };
    (back_off && backoff_step(&self.cancel, &self.drain, backoff).await).then_some(Ok(()))
  }

  /// The `/runner/*` client for this pass: the registration's API origin and
  /// its host token, registered with the masker whenever it changes.
  fn toolu_client(
    &self,
    cfg: &RunnerRegistrationConfig,
    client: &reqwest::Client,
    masked_token: &mut String,
  ) -> Result<TooluClient, Box<dyn std::error::Error>> {
    let ProviderSection::Toolu { api_url, .. } = &cfg.provider else {
      return Err(
        format!(
          "{} is not a toolu.sh registration",
          self.config_path.display()
        )
        .into(),
      );
    };
    let creds = load_credentials(&self.creds_path).map_err(|e| format!("{e}"))?;
    if creds.access_token != *masked_token {
      self
        .masker
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .add_secret(&creds.access_token);
      masked_token.clone_from(&creds.access_token);
    }
    TooluClient::new(client.clone(), api_url, &creds.access_token).map_err(|e| {
      format!(
        "{e} ({}) — pair the machine again with `toolu-runner setup`",
        self.config_path.display()
      )
      .into()
    })
  }

  /// List the queue and claim the first job this machine wins; run it.
  async fn poll_once(
    &self,
    cfg: &RunnerRegistrationConfig,
    client: &reqwest::Client,
    toolu: &TooluClient,
  ) -> Result<Pass, Box<dyn std::error::Error>> {
    let queued = match toolu.queued().await {
      Ok(queued) => queued,
      Err(TooluError::Revoked(why)) => return Err(revoked(&why)),
      Err(e @ (TooluError::Conflict(_) | TooluError::NotFound | TooluError::Transient(_))) => {
        tracing::warn!(error = %e, "listing toolu.sh jobs failed; backing off");
        return Ok(Pass::Backoff);
      },
      Err(e @ TooluError::Protocol(_)) => {
        tracing::warn!(error = %e, "toolu.sh answered the job list unexpectedly; backing off");
        return Ok(Pass::Backoff);
      },
    };
    for job in &queued.jobs {
      let claim = match toolu.claim(&job.id).await {
        Ok(claim) => claim,
        Err(refusal) => match claim_refused(&job.id, refusal)? {
          Some(pass) => return Ok(pass),
          None => continue,
        },
      };
      let claimed = ClaimedJob {
        toolu,
        client,
        cfg,
        masker: &self.masker,
        cancel: &self.cancel,
        conflict_policy: self.conflict_policy,
      };
      return Ok(Pass::Ran(claimed.run(&job.id, claim).await?));
    }
    Ok(Pass::Idle)
  }
}

/// What a refused claim of `job` means: `None` to try the next listed job
/// (a lost race, a job gone), else the pass to end with; `Err` when the
/// machine was revoked.
fn claim_refused(
  job: &str,
  refusal: TooluError,
) -> Result<Option<Pass>, Box<dyn std::error::Error>> {
  match refusal {
    TooluError::NotFound => {
      tracing::info!(%job, "the job is no longer claimable; trying the next");
      Ok(None)
    },
    TooluError::Conflict(body) => match parse_claim_refusal(&body) {
      ClaimRefusal::Taken => {
        tracing::info!(%job, "another machine claimed the job first; trying the next");
        Ok(None)
      },
      ClaimRefusal::SlotsFull => {
        tracing::info!("toolu.sh says this machine's slots are full; waiting");
        Ok(Some(Pass::Idle))
      },
      ClaimRefusal::Other(reason) => {
        tracing::warn!(%job, %reason, "toolu.sh refused the claim; waiting");
        Ok(Some(Pass::Idle))
      },
    },
    TooluError::Revoked(why) => Err(revoked(&why)),
    e @ (TooluError::Transient(_) | TooluError::Protocol(_)) => {
      tracing::warn!(%job, error = %e, "claiming the job failed; backing off");
      Ok(Some(Pass::Backoff))
    },
  }
}

/// The fatal error of a 401: the machine was revoked (or never paired).
pub(crate) fn revoked(why: &str) -> Box<dyn std::error::Error> {
  format!(
    "toolu.sh rejected this machine's host token (401: {why}) — the machine was revoked or is \
     not paired; pair it again with `toolu-runner setup`"
  )
  .into()
}
