//! `run` for a toolu.sh machine (`[provider] kind = "toolu"`, #206): list
//! toolu.sh's `/runner/*` queue, claim one job, run its JIT config once
//! ([`crate::poll_job`]), repeat. Contract: CodaSignal/toolu.sh
//! `docs/guides/self-hosted-machines.md`; lock, cancel and drain as `run`.
//! A 401 is fatal (revoked); 429/5xx/transport back off 1 s → 60 s (reset
//! when a poll finds nothing to claim or a job ends without a transient
//! error); a lost race or an off-contract claim reply moves to the next job.
//! Config and host token reload every pass; the HTTP client only when
//! `[network]` changes.

use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use config::config::{RunnerRegistrationConfig, load_credentials, resolve_data_dir};
use config::provider::ProviderSection;
use listener::loop_decision::{LoopAction, next_action};
use listener::session_conflict::SessionConflictPolicy;
use protocol::toolu_jobs::{ClaimRefusal, QueuedJobs, parse_claim_refusal};
use shared::{Conclusion, NetworkConfig, RunnerError, SecretMasker};
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

/// What one pass carries over to the next.
#[derive(Default)]
struct PassState {
  /// Logs and masks a `[network]` change once.
  network_memo: crate::network_setup::NetworkMemo,
  /// The host token last registered with the masker.
  masked_token: String,
  /// The HTTP client and the `[network]` it was built for.
  client: Option<(NetworkConfig, reqwest::Client)>,
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
    let mut state = PassState::default();
    loop {
      if self.stopping() || self.pending_path.exists() {
        tracing::info!("stopping the toolu.sh poll loop between jobs");
        return Ok(());
      }
      // Bound first: a `?` temporary in the scrutinee would live across the
      // `await`s below, making the loop future `!Send` (`run --all`).
      let pass = self.pass(&mut state).await?;
      if let Some(end) = self.settle(pass, &mut backoff).await {
        return end;
      }
    }
  }

  /// Cancelled or drained: claim nothing more.
  fn stopping(&self) -> bool {
    self.cancel.is_cancelled() || self.drain.is_cancelled()
  }

  /// One pass on the freshly reloaded config: the clients, then list and
  /// claim ([`Self::poll_once`]).
  async fn pass(&self, state: &mut PassState) -> Result<Pass, Box<dyn std::error::Error>> {
    let cfg = load_run_config(&self.config_path)?;
    let client = state.client(&cfg, &self.masker)?;
    let toolu = state.toolu(
      &cfg,
      &client,
      &self.masker,
      (&self.config_path, &self.creds_path),
    )?;
    self.poll_once(&cfg, &client, &toolu).await
  }

  /// Wait as `pass` calls for; `Some` ends the loop with that result.
  async fn settle(
    &self,
    pass: Pass,
    backoff: &mut Duration,
  ) -> Option<Result<(), Box<dyn std::error::Error>>> {
    let wait = match pass {
      Pass::Idle => {
        *backoff = BACKOFF_START;
        return sleep_or_stop(&self.cancel, &self.drain, self.poll_interval)
          .await
          .then_some(Ok(()));
      },
      Pass::Backoff => true,
      Pass::Ran(outcome) => {
        let outcome = outcome.map(|_conclusion| ());
        match next_action(
          &outcome,
          self.once,
          self.stopping(),
          self.pending_path.exists(),
        ) {
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
    (wait && backoff_step(&self.cancel, &self.drain, backoff).await).then_some(Ok(()))
  }

  /// List the queue and claim the first job this machine wins; run it.
  async fn poll_once(
    &self,
    cfg: &RunnerRegistrationConfig,
    client: &reqwest::Client,
    toolu: &TooluClient,
  ) -> Result<Pass, Box<dyn std::error::Error>> {
    let queued = match self.list(toolu).await? {
      ControlFlow::Continue(queued) => queued,
      ControlFlow::Break(pass) => return Ok(pass),
    };
    for job in &queued.jobs {
      // A claim cannot be handed back: once stopping, claim nothing more.
      if self.stopping() {
        return Ok(Pass::Idle);
      }
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

  /// `GET /runner/jobs/queued`, abandoned on cancel or drain: the jobs, or
  /// the pass to end with.
  async fn list(
    &self,
    toolu: &TooluClient,
  ) -> Result<ControlFlow<Pass, QueuedJobs>, Box<dyn std::error::Error>> {
    let listed = tokio::select! {
      () = self.cancel.cancelled() => return Ok(ControlFlow::Break(Pass::Idle)),
      () = self.drain.cancelled() => return Ok(ControlFlow::Break(Pass::Idle)),
      listed = toolu.queued() => listed,
    };
    match listed {
      Ok(queued) => Ok(ControlFlow::Continue(queued)),
      Err(TooluError::Revoked(why)) => Err(revoked(&why)),
      Err(e) => {
        warn_list_failed(&e);
        Ok(ControlFlow::Break(Pass::Backoff))
      },
    }
  }
}

impl PassState {
  /// The pass's HTTP client: the previous pass's while `[network]` is
  /// unchanged (idle polls reuse one connection), else a fresh one.
  fn client(
    &mut self,
    cfg: &RunnerRegistrationConfig,
    masker: &Mutex<SecretMasker>,
  ) -> Result<reqwest::Client, RunnerError> {
    let network = cfg.network_config();
    if let Some(client) = self
      .client
      .as_ref()
      .filter(|(built_for, _)| *built_for == network)
      .map(|(_, client)| client.clone())
    {
      return Ok(client);
    }
    let client = crate::network_setup::lifecycle_client(
      &network,
      masker,
      LOOP_CLIENT_TIMEOUT,
      &mut self.network_memo,
    )?;
    self.client = Some((network, client.clone()));
    Ok(client)
  }

  /// The `/runner/*` client for this pass: the registration's API origin and
  /// its host token (`paths` = its `config.toml`, `credentials.json`),
  /// registered with the masker whenever it changes.
  fn toolu(
    &mut self,
    cfg: &RunnerRegistrationConfig,
    client: &reqwest::Client,
    masker: &Mutex<SecretMasker>,
    (config_path, creds_path): (&Path, &Path),
  ) -> Result<TooluClient, String> {
    let ProviderSection::Toolu { api_url, .. } = &cfg.provider else {
      return Err(format!(
        "{} is not a toolu.sh registration",
        config_path.display()
      ));
    };
    let creds = load_credentials(creds_path).map_err(|e| format!("{e}"))?;
    if creds.access_token != self.masked_token {
      masker
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .add_secret(&creds.access_token);
      self.masked_token.clone_from(&creds.access_token);
    }
    TooluClient::new(client.clone(), api_url, &creds.access_token).map_err(|e| {
      format!(
        "{e} ({}) — pair the machine again with `toolu-runner setup`",
        config_path.display()
      )
    })
  }
}

/// Log a failed job list (the pass backs off).
fn warn_list_failed(e: &TooluError) {
  let what = if matches!(e, TooluError::Protocol(_) | TooluError::Refused(_)) {
    "toolu.sh answered the job list unexpectedly; backing off"
  } else {
    "listing toolu.sh jobs failed; backing off"
  };
  tracing::warn!(error = %e, "{what}");
}

/// What a refused claim of `job` means: `None` to try the next listed job
/// (a lost race, a job gone, a reply outside the contract), else the pass to end with; `Err` when the
/// machine was revoked.
fn claim_refused(
  job: &str,
  refusal: TooluError,
) -> Result<Option<Pass>, Box<dyn std::error::Error>> {
  let (next, what) = match refusal {
    TooluError::Revoked(why) => return Err(revoked(&why)),
    TooluError::NotFound => (
      None,
      "the job is no longer claimable; trying the next".to_owned(),
    ),
    TooluError::Conflict(body) => match parse_claim_refusal(&body) {
      ClaimRefusal::Taken => (
        None,
        "another machine claimed the job first; trying the next".to_owned(),
      ),
      ClaimRefusal::SlotsFull => (
        Some(Pass::Idle),
        "toolu.sh says this machine's slots are full; waiting".to_owned(),
      ),
      ClaimRefusal::Other(reason) => (
        Some(Pass::Idle),
        format!("toolu.sh refused the claim ({reason}); waiting"),
      ),
    },
    // A claim reply outside the contract is about this job: skip it, so one
    // bad job cannot block the rest of the queue.
    e @ (TooluError::Protocol(_) | TooluError::Refused(_)) => (
      None,
      format!("toolu.sh answered the claim unexpectedly ({e}); trying the next"),
    ),
    e @ TooluError::Transient(_) => (
      Some(Pass::Backoff),
      format!("claiming the job failed ({e}); backing off"),
    ),
  };
  tracing::info!(%job, "{what}");
  Ok(next)
}

/// The fatal error of a 401: the machine was revoked (or never paired).
pub(crate) fn revoked(why: &str) -> Box<dyn std::error::Error> {
  format!(
    "toolu.sh rejected this machine's host token (401: {why}) — the machine was revoked or is \
     not paired; pair it again with `toolu-runner setup`"
  )
  .into()
}
