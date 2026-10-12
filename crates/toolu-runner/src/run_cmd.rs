//! `run` subcommand: the always-online JIT re-mint loop.
//!
//! A JIT config is single-use, so per iteration `run` reloads `config.toml`,
//! runs one listener lifecycle, then dispatches [`next_action`]: re-mint on a
//! completed job, cancel-aware jittered backoff on a transient failure, exit
//! on cancel / `--once` / `.pending_remove`. An `--ephemeral` registration
//! never re-mints: after its one job it is unregistered and deleted
//! ([`crate::ephemeral`]). Extracted from `main.rs`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use config::auth_store::{self, AuthStore};
use config::config::{
  RunnerRegistrationConfig, load_config as load_reg_config, load_credentials, resolve_data_dir,
  resolve_work_dir,
};
use config::{lockfile, registry};
use listener::GitHubListener;
use listener::loop_decision::{
  EphemeralAction, LoopAction, LoopSignals, next_action, next_ephemeral_action,
};
use listener::session_conflict::SessionConflictPolicy;
use shared::{Conclusion, RunnerError, SESSION_CONFLICT_EXIT_CODE, SecretMasker};
use tokio_util::sync::CancellationToken;

use crate::cli::{RunArgs, credentials_path_for};
use crate::register_cmd;

/// Backoff floor: the first retry sleeps ~this, reset after any success.
const BACKOFF_START: Duration = Duration::from_secs(1);
/// Backoff ceiling: doubling never exceeds this.
const BACKOFF_MAX: Duration = Duration::from_secs(60);
/// Request timeout of each lifecycle's listener / re-mint client.
const LOOP_CLIENT_TIMEOUT: Duration = Duration::from_secs(60);

/// Guidance for the two fatal re-mint token paths (no stored bearer, or a
/// bearer GitHub rejected): names every way to supply a fresh token.
const REMINT_TOKEN_HELP: &str = "re-mint needs a GitHub token: run `toolu-runner login`, pass \
                                 --token to `toolu-runner register`, or set TOOLU_RUNNER_TOKEN";

/// `run`: acquire the single-job lock, then drive the always-online loop.
///
/// Startup (once): init tracing, resolve + load the config, acquire the
/// per-repo `.lock`, bridge SIGINT/SIGTERM to a [`CancellationToken`], and
/// WARN (unless `--once`) when no login token is stored — the runner would
/// then exit after the first job. The lock is held for the loop's lifetime.
pub(crate) async fn cmd_run(args: RunArgs) -> Result<(), Box<dyn std::error::Error>> {
  let masker = Arc::new(Mutex::new(SecretMasker::new()));
  crate::init_tracing_for(&masker).map_err(|e| format!("startup init: {e}"))?;

  let config_path = crate::resolve_config(args.config)?;
  let cfg = load_run_config(&config_path)?;
  let data_dir = resolve_data_dir(&cfg.runtime.data_dir).map_err(|e| format!("{e}"))?;
  let lock_path = data_dir.join(".lock");
  // Held until `cmd_run` returns — including across an ephemeral cleanup,
  // which deletes the `.lock` path while still holding it (as `remove` does).
  let _lock_guard = lockfile::acquire(&lock_path, &config_path).map_err(|e| format!("{e}"))?;
  tracing::info!(path = %lock_path.display(), "acquired single-job lock");

  let cancel = CancellationToken::new();
  spawn_signal_bridge(cancel.clone());

  let store = AuthStore::new(&registry::runner_home());
  let host = register_cmd::host_from_runner_url(&cfg.runner_url)?;
  // An ephemeral registration never re-mints, so it needs no login token.
  warn_if_no_login(&store, &host, args.once || cfg.ephemeral);

  RunLoop {
    config_path: config_path.clone(),
    creds_path: credentials_path_for(&config_path),
    pending_path: data_dir.join(".pending_remove"),
    lock_path,
    masker,
    store,
    host,
    cancel,
    once: args.once,
    conflict_policy: crate::config::session_conflict_policy(),
  }
  .drive()
  .await
  // `_lock_guard` drops here, releasing the lock.
}

/// WARN once, before polling, when the runner will drop offline after the
/// first job: no `--once` and no resolvable login token means re-mint has no
/// bearer, so the loop exits after one job. `--once` opts out by design.
fn warn_if_no_login(store: &AuthStore, host: &str, once: bool) {
  if once {
    return;
  }
  match auth_store::resolve_bearer(store, host, None) {
    Ok(Some(_)) => {},
    Ok(None) => tracing::warn!(
      %host,
      "no stored GitHub login; the runner will exit after the first job — run \
       `toolu-runner login` to stay online across jobs (or pass --once to silence this); \
       a login stored in the OS keyring by an older version is no longer read by \
       default — re-run `toolu-runner login`, or set TOOLU_RUNNER_KEYRING=1"
    ),
    Err(e) => tracing::warn!(error = %e, "could not check for a stored GitHub login"),
  }
}

/// Loop-invariant handles for the always-online run loop; the config
/// itself is reloaded from `config_path` on every iteration.
struct RunLoop {
  config_path: PathBuf,
  creds_path: PathBuf,
  pending_path: PathBuf,
  /// The job `.lock` this process holds (deleted by an ephemeral cleanup).
  lock_path: PathBuf,
  masker: Arc<Mutex<SecretMasker>>,
  store: AuthStore,
  host: String,
  cancel: CancellationToken,
  once: bool,
  /// Session-conflict retry schedule handed to every listener lifecycle.
  conflict_policy: SessionConflictPolicy,
}

impl RunLoop {
  /// Drive the loop until cancel, `--once`, `.pending_remove`, or a fatal
  /// error. Reloads the config each iteration so a re-mint (or a user edit)
  /// takes effect on the next pass. Backoff resets after any successful job
  /// or re-mint and is capped at [`BACKOFF_MAX`].
  async fn drive(self) -> Result<(), Box<dyn std::error::Error>> {
    let mut backoff = BACKOFF_START;
    loop {
      let cfg = load_run_config(&self.config_path)?;
      // Rebuilt from the reloaded `[network]` every pass (edits apply next
      // pass); an unusable value is fatal like a corrupt config. Shared by
      // the listener, the re-mint and an ephemeral unregister. 60 s: the
      // listener's per-client timeout; `register_jit` sets its own 30 s.
      let client = crate::network_setup::lifecycle_client(
        &cfg.network_config(),
        &self.masker,
        LOOP_CLIENT_TIMEOUT,
      )?;
      // Fatal setup errors (corrupt config, unparseable JIT) deliberately
      // exit here without backoff — only listener outcomes are retried.
      let (raw, job_acquired) = self.run_once(&cfg, &client).await?;
      let cancelled = self.cancel.is_cancelled();
      let pending_remove = self.pending_path.exists();
      if cfg.ephemeral {
        let signals = LoopSignals {
          once: self.once,
          cancelled,
          pending_remove,
        };
        match next_ephemeral_action(&raw, job_acquired, signals) {
          EphemeralAction::Unregister => return self.finish_ephemeral(&cfg, &client, raw).await,
          EphemeralAction::Exit(code) => return finish_ephemeral_exit(code, raw),
          EphemeralAction::BackoffRetry if self.backoff_step(&mut backoff).await => return Ok(()),
          EphemeralAction::BackoffRetry => continue,
        }
      }
      // The persistent loop only distinguishes Ok from Err; the completed
      // job's `Conclusion` is not needed by `next_action`.
      let outcome = raw.map(|_conclusion| ());
      match next_action(&outcome, self.once, cancelled, pending_remove) {
        LoopAction::Exit(code) => return finish(code, outcome),
        LoopAction::Reregister => {
          // A completed job is proof of health — shed backoff accumulated
          // by earlier transient failures before the re-mint attempt.
          if outcome.is_ok() {
            backoff = BACKOFF_START;
          }
          match self.reregister(&cfg, &client).await? {
            Remint::Reset => backoff = BACKOFF_START,
            Remint::Retry if self.backoff_step(&mut backoff).await => return Ok(()),
            Remint::Retry => {},
          }
        },
        LoopAction::BackoffRetry if self.backoff_step(&mut backoff).await => return Ok(()),
        LoopAction::BackoffRetry => {},
      }
    }
  }

  /// Build a listener from `cfg` and run one JIT lifecycle. The outer
  /// `Result` is fatal setup (bad config / unparseable JIT blob / listener
  /// init); the inner is the listener's own outcome — `Ok(Some(_))` means a
  /// job ran — paired with whether a job was acquired at all (true even when
  /// the lifecycle then errored), which an ephemeral registration needs.
  async fn run_once(
    &self,
    cfg: &RunnerRegistrationConfig,
    client: &reqwest::Client,
  ) -> Result<(Result<Option<Conclusion>, RunnerError>, bool), Box<dyn std::error::Error>> {
    let runner_cfg = build_runner_config(cfg)?;
    let jit = require_jit_config(cfg)?;
    let listener = GitHubListener::new(&jit, runner_cfg, Arc::clone(&self.masker), client.clone())
      .map_err(|e| format!("listener init: {e}"))?
      .with_session_conflict_policy(self.conflict_policy);
    let outcome = listener.run(self.cancel.clone()).await;
    Ok((outcome, listener.job_acquired()))
  }

  /// An ephemeral registration's job ran: unregister it and delete the local
  /// state ([`crate::ephemeral::finish_job`]). When the lifecycle itself
  /// errored after the acquire (execution or completion failed), the cleanup
  /// still runs and `run` then exits 1 naming that error.
  async fn finish_ephemeral(
    &self,
    cfg: &RunnerRegistrationConfig,
    client: &reqwest::Client,
    outcome: Result<Option<Conclusion>, RunnerError>,
  ) -> Result<(), Box<dyn std::error::Error>> {
    let state = crate::ephemeral::LocalState {
      config: &self.config_path,
      creds: &self.creds_path,
      pending: &self.pending_path,
      lock: &self.lock_path,
    };
    if let Err(cleanup) = crate::ephemeral::finish_job(cfg, &state, client).await {
      return Err(match &outcome {
        Ok(_) => cleanup,
        Err(e) => format!("{cleanup}\n(the job itself ended with: listener: {e})").into(),
      });
    }
    match outcome {
      Ok(_) => Ok(()),
      Err(e) => Err(Box::new(CodedExit {
        code: 1,
        message: format!("listener: {e} (the job was acquired; its registration was removed)"),
      })),
    }
  }

  /// Re-mint a fresh JIT config for the next iteration. Re-resolves the
  /// bearer (a `login` may have rotated it); a missing bearer or an
  /// auth-rejected mint is fatal (naming [`REMINT_TOKEN_HELP`]), any other
  /// mint failure backs off and retries the still-valid current config.
  async fn reregister(
    &self,
    cfg: &RunnerRegistrationConfig,
    client: &reqwest::Client,
  ) -> Result<Remint, Box<dyn std::error::Error>> {
    let Some(bearer) = auth_store::resolve_bearer(&self.store, &self.host, None)? else {
      return Err(format!("no GitHub token for {} — {REMINT_TOKEN_HELP}", self.host).into());
    };
    match register_cmd::remint_and_persist(
      cfg,
      &bearer,
      &self.config_path,
      &self.creds_path,
      client,
    )
    .await
    {
      Ok(()) => Ok(Remint::Reset),
      Err(RunnerError::Auth(msg)) => {
        Err(format!("re-mint rejected: {msg} — {REMINT_TOKEN_HELP}").into())
      },
      Err(e) => {
        tracing::warn!(error = %e, "re-mint failed; backing off before retry");
        Ok(Remint::Retry)
      },
    }
  }

  /// Sleep the current backoff (cancel-aware), then double it toward the
  /// cap. Returns `true` when cancellation fired during the sleep, so the
  /// caller exits 0.
  async fn backoff_step(&self, backoff: &mut Duration) -> bool {
    if self.sleep_or_cancel(*backoff).await {
      return true;
    }
    *backoff = next_backoff(*backoff);
    false
  }

  /// Sleep `backoff` with jitter applied (see [`jittered_backoff`]),
  /// waking early on cancel. Returns `true` if cancellation fired first.
  async fn sleep_or_cancel(&self, backoff: Duration) -> bool {
    let jittered = jittered_backoff(backoff);
    tokio::select! {
      () = self.cancel.cancelled() => {
        tracing::info!("cancelled during backoff; shutting down");
        true
      },
      () = tokio::time::sleep(jittered) => false,
    }
  }
}

/// Outcome of a re-mint attempt: reset the backoff, or back off and retry.
enum Remint {
  /// Mint + persist succeeded — reset the backoff and loop.
  Reset,
  /// Transient (non-auth) mint failure — back off, then retry the config.
  Retry,
}

/// Turn a [`LoopAction::Exit`] code into the process result. Code 0 is a
/// clean exit; the session-conflict code paired with an actual
/// `SessionConflict` reaches `main` as a [`CodedExit`]; any other non-zero
/// code (`--once` over a failed job) surfaces the listener error for `main`'s
/// generic non-zero exit.
fn finish(code: i32, outcome: Result<(), RunnerError>) -> Result<(), Box<dyn std::error::Error>> {
  if code == 0 {
    return Ok(());
  }
  match outcome {
    Err(e @ RunnerError::SessionConflict(_)) if code == SESSION_CONFLICT_EXIT_CODE => {
      Err(Box::new(CodedExit {
        code,
        message: format!("listener: {e}"),
      }))
    },
    Err(e) => Err(format!("listener: {e}").into()),
    Ok(()) => Ok(()),
  }
}

/// Turn an ephemeral [`EphemeralAction::Exit`] into the process result: the
/// registration was not used, so it is kept. A non-zero code reaches `main`
/// as a [`CodedExit`] (exactly 1 or 5), naming `toolu-runner remove` for a
/// registration that may be dead.
fn finish_ephemeral_exit(
  code: i32,
  outcome: Result<Option<Conclusion>, RunnerError>,
) -> Result<(), Box<dyn std::error::Error>> {
  if code == 0 {
    return Ok(());
  }
  let reason = outcome
    .err()
    .map_or_else(|| "no job".to_owned(), |e| e.to_string());
  Err(Box::new(CodedExit {
    code,
    message: format!(
      "listener: {reason}\nthe ephemeral registration was kept — if the runner was removed on \
       GitHub, run `toolu-runner remove` to clean it up"
    ),
  }))
}

/// A fatal `run` error that must exit with a specific process code instead
/// of `main`'s generic 2 (today only the session conflict's
/// [`SESSION_CONFLICT_EXIT_CODE`]).
#[derive(Debug)]
pub(crate) struct CodedExit {
  /// The process exit code `main` applies.
  pub(crate) code: i32,
  /// What `main` prints to stderr.
  message: String,
}

impl std::fmt::Display for CodedExit {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str(&self.message)
  }
}

impl std::error::Error for CodedExit {}

/// Map a freshly loaded registration into the engine's runtime config.
///
/// Resolved per iteration so a re-mint's preserved `[services]`/`[cache]`/
/// `[workspace]`/`[shadow]` sections (and any user edit) take effect.
fn build_runner_config(
  cfg: &RunnerRegistrationConfig,
) -> Result<shared::RunnerConfig, Box<dyn std::error::Error>> {
  let data_dir = resolve_data_dir(&cfg.runtime.data_dir).map_err(|e| format!("{e}"))?;
  Ok(shared::RunnerConfig {
    data_dir,
    workspace_root: resolve_work_dir(&cfg.runtime.work_dir),
    cgroup_path: None,
    services_mode: cfg.services_mode(),
    service_bind: cfg.service_bind(),
    cache: cfg.cache_config(),
    workspace_gc_hours: cfg.workspace_gc_hours(),
    workspace: cfg.workspace_config(),
    shadow_enabled: cfg.shadow_enabled(),
    node_dir: crate::config::node_dir(),
    network: cfg.network_config(),
  })
}

/// Double `current` toward [`BACKOFF_MAX`] without overflowing.
fn next_backoff(current: Duration) -> Duration {
  current.saturating_mul(2).min(BACKOFF_MAX)
}

/// Decorrelated jitter so concurrent runners don't synchronize retries:
/// returns a duration in `[d/2, d)`. Mirrors the listener's private helper
/// (`job_lifecycle::jittered_backoff`); duplicated because it is
/// `SessionCtx`-free here and the listener one is private.
fn jittered_backoff(d: Duration) -> Duration {
  let Ok(half_ms) = u64::try_from(d.as_millis().saturating_div(2)) else {
    return d;
  };
  if half_ms == 0 {
    return d;
  }
  let jitter = fastrand::u64(0..half_ms);
  Duration::from_millis(half_ms + jitter)
}

/// Bridge SIGINT/SIGTERM to `cancel` in a background task. Shared with
/// `boot_cmd`, which mirrors `run`'s signal handling for the one-shot
/// container path.
pub(crate) fn spawn_signal_bridge(cancel: CancellationToken) {
  tokio::spawn(async move {
    let Ok(mut sigint) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
    else {
      return;
    };
    let Ok(mut sigterm) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
    else {
      return;
    };
    tokio::select! {
      _ = sigint.recv() => {},
      _ = sigterm.recv() => {},
    }
    cancel.cancel();
  });
}

/// Load + validate the persisted config and credentials for `run`.
///
/// Errors if either file is missing (with a `register` hint) or unparseable.
fn load_run_config(
  config_path: &std::path::Path,
) -> Result<RunnerRegistrationConfig, Box<dyn std::error::Error>> {
  if !config_path.exists() {
    return Err(
      format!(
        "config not found at {} — run `toolu-runner register` first",
        config_path.display()
      )
      .into(),
    );
  }
  let creds_path = credentials_path_for(config_path);
  if !creds_path.exists() {
    return Err(
      format!(
        "credentials not found at {} — run `toolu-runner register` first",
        creds_path.display()
      )
      .into(),
    );
  }
  let cfg = load_reg_config(config_path).map_err(|e| format!("{e}"))?;
  let _creds = load_credentials(&creds_path).map_err(|e| format!("{e}"))?;
  Ok(cfg)
}

/// Lift the JIT config blob out of the persisted config (written by
/// `register` via generate-jitconfig). An empty blob means the config
/// predates live registration — re-run `register`.
fn require_jit_config(
  cfg: &RunnerRegistrationConfig,
) -> Result<String, Box<dyn std::error::Error>> {
  let blob = cfg.runtime.jit_config.clone();
  if blob.is_empty() {
    return Err(
      "config.toml has no JIT config blob — re-run `toolu-runner register` against a live GH repo"
        .into(),
    );
  }
  Ok(blob)
}
