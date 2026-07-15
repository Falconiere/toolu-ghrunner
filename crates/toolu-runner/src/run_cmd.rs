//! `run` subcommand: the always-online JIT re-mint loop.
//!
//! A JIT config is single-use, so per iteration `run` reloads `config.toml`,
//! runs one listener lifecycle, then dispatches [`next_action`]: re-mint on a
//! completed job, cancel-aware jittered backoff on a transient failure, exit
//! on cancel / `--once` / `.pending_remove`. Extracted from `main.rs`.

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
use listener::loop_decision::{LoopAction, next_action};
use shared::{RunnerError, SecretMasker};
use tokio_util::sync::CancellationToken;

use crate::cli::{RunArgs, credentials_path_for};
use crate::register_cmd;

/// Backoff floor: the first retry sleeps ~this, reset after any success.
const BACKOFF_START: Duration = Duration::from_secs(1);
/// Backoff ceiling: doubling never exceeds this.
const BACKOFF_MAX: Duration = Duration::from_secs(60);

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
  let _lock_guard = lockfile::acquire(&lock_path, &config_path).map_err(|e| format!("{e}"))?;
  tracing::info!(path = %lock_path.display(), "acquired single-job lock");

  let cancel = CancellationToken::new();
  spawn_signal_bridge(cancel.clone());

  let store = AuthStore::new(&registry::runner_home());
  let host = register_cmd::host_from_runner_url(&cfg.runner_url)?;
  warn_if_no_login(&store, &host, args.once);

  RunLoop {
    config_path: config_path.clone(),
    creds_path: credentials_path_for(&config_path),
    pending_path: data_dir.join(".pending_remove"),
    masker,
    store,
    host,
    cancel,
    once: args.once,
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
       `toolu-runner login` to stay online across jobs (or pass --once to silence this)"
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
  masker: Arc<Mutex<SecretMasker>>,
  store: AuthStore,
  host: String,
  cancel: CancellationToken,
  once: bool,
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
      // Fatal setup errors (corrupt config, unparseable JIT) deliberately
      // exit here without backoff — only listener outcomes are retried.
      let outcome = self.run_once(&cfg).await?;
      let cancelled = self.cancel.is_cancelled();
      let pending_remove = self.pending_path.exists();
      match next_action(&outcome, self.once, cancelled, pending_remove) {
        LoopAction::Exit(code) => return finish(code, outcome),
        LoopAction::Reregister => {
          // A completed job is proof of health — shed backoff accumulated
          // by earlier transient failures before the re-mint attempt.
          if outcome.is_ok() {
            backoff = BACKOFF_START;
          }
          match self.reregister(&cfg).await? {
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
  /// init); the inner is the listener's own outcome for [`next_action`].
  async fn run_once(
    &self,
    cfg: &RunnerRegistrationConfig,
  ) -> Result<Result<(), RunnerError>, Box<dyn std::error::Error>> {
    let runner_cfg = build_runner_config(cfg)?;
    let jit = require_jit_config(cfg)?;
    let listener = GitHubListener::new(&jit, runner_cfg, Arc::clone(&self.masker))
      .map_err(|e| format!("listener init: {e}"))?;
    Ok(listener.run(self.cancel.clone()).await)
  }

  /// Re-mint a fresh JIT config for the next iteration. Re-resolves the
  /// bearer (a `login` may have rotated it); a missing bearer or an
  /// auth-rejected mint is fatal (naming [`REMINT_TOKEN_HELP`]), any other
  /// mint failure backs off and retries the still-valid current config.
  async fn reregister(
    &self,
    cfg: &RunnerRegistrationConfig,
  ) -> Result<Remint, Box<dyn std::error::Error>> {
    let Some(bearer) = auth_store::resolve_bearer(&self.store, &self.host, None)? else {
      return Err(REMINT_TOKEN_HELP.into());
    };
    match register_cmd::remint_and_persist(cfg, &bearer, &self.config_path, &self.creds_path).await
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
/// clean exit; a non-zero code arises only from `--once` over a failed job,
/// so surface the listener error for `main` to map to a non-zero exit.
fn finish(code: i32, outcome: Result<(), RunnerError>) -> Result<(), Box<dyn std::error::Error>> {
  if code == 0 {
    return Ok(());
  }
  outcome.map_err(|e| format!("listener: {e}"))?;
  Ok(())
}

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
    shadow_enabled: cfg.shadow_enabled(),
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

/// Bridge SIGINT/SIGTERM to `cancel` in a background task.
fn spawn_signal_bridge(cancel: CancellationToken) {
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
