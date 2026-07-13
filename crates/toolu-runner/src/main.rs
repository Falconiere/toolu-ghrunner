//! `toolu-runner` — standalone GitHub Actions JIT runner CLI.
//!
//! Subcommands: `register` (live `generate-jitconfig`, persists real
//! jit_config + runner_id), `run` (load config, hold `.lock`, run the
//! listener until SIGINT/SIGTERM), `remove` (delete state or write
//! `.pending_remove` mid-job), `status` (print config, no network),
//! `watch` (TUI over the job journal, no network).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::Parser;
use config::config::{
  RunnerRegistrationConfig, load_config as load_reg_config, load_credentials, resolve_data_dir,
  resolve_work_dir,
};
use config::lockfile;
use config::{registry, repo_infer};
use listener::GitHubListener;
use shared::RunnerError;
use shared::startup;
use shared::{MaskerRedactor, SecretMasker};
use tokio_util::sync::CancellationToken;

mod cli;
mod login_cmd;
mod register_cmd;
mod status_cmd;

use crate::cli::{
  Cli, Command, RemoveArgs, RunArgs, WatchArgs, credentials_path_for, default_config_path,
};

#[tokio::main]
async fn main() {
  #[cfg(debug_assertions)]
  cli::debug_assert_cli();
  let cli = Cli::parse();
  let exit_code = match run(cli).await {
    Ok(()) => 0,
    Err(err) => {
      eprintln!("toolu-runner: {err}");
      2
    },
  };
  std::process::exit(exit_code);
}

async fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
  match cli.command {
    Command::Register(args) => register_cmd::cmd_register(args).await,
    Command::Run(args) => cmd_run(args).await,
    Command::Remove(args) => cmd_remove(args).await,
    Command::Status(args) => status_cmd::cmd_status(args),
    Command::Watch(args) => cmd_watch(args),
    Command::Login(args) => login_cmd::cmd_login(args).await,
    Command::Logout(args) => login_cmd::cmd_logout(&args),
  }
}

/// `watch`: TUI over the job journal. Blocks until the user quits; no
/// tracing init so log output never corrupts the alternate screen.
///
/// Resolution is tolerant: when no registration resolves (none yet, or
/// several without a cwd match), fall back to the default
/// `<home>/config.toml` path — `run_watch` browses every discovered
/// runner dir (plus the legacy home) when that file does not load, so
/// history browsing still works unregistered.
fn cmd_watch(args: WatchArgs) -> Result<(), Box<dyn std::error::Error>> {
  let config_path = match resolve_config(args.config) {
    Ok(path) => path,
    Err(_) => default_config_path(),
  };
  observability::watch::run_watch(&config_path)?;
  Ok(())
}

/// Resolve which registration config a subcommand should use: the
/// `--config` flag > the cwd-inferred `runners/<owner>/<repo>/`
/// registration (github.com `origin` remotes only — GHES and ssh-alias
/// hosts never infer; inference is one local `git remote get-url origin`
/// subprocess, no network) > the sole existing registration (the legacy
/// `<home>/config.toml` included). Zero registrations or an ambiguous
/// set propagates [`registry::resolve_config_path`]'s error as-is.
fn resolve_config(flag: Option<PathBuf>) -> Result<PathBuf, Box<dyn std::error::Error>> {
  // An explicit flag short-circuits before the git shell-out: a `--config`
  // invocation must work even where cwd inference cannot run.
  if let Some(path) = flag {
    return Ok(path);
  }
  let cwd = std::env::current_dir()?;
  let inferred = repo_infer::detect_repo(&cwd)
    .ok()
    .filter(|repo| repo.host.eq_ignore_ascii_case("github.com"));
  let owner_repo = inferred
    .as_ref()
    .map(|repo| (repo.owner.as_str(), repo.repo.as_str()));
  Ok(registry::resolve_config_path(
    None,
    &registry::runner_home(),
    owner_repo,
  )?)
}

/// Register `masker` as the tracing secret-redactor and initialize tracing.
fn init_tracing_for(masker: &Arc<std::sync::Mutex<SecretMasker>>) -> Result<(), RunnerError> {
  let redactor: Arc<dyn shared::startup::SecretRedactor> =
    Arc::new(MaskerRedactor(Arc::clone(masker)));
  startup::init_with_redactor(env!("CARGO_MANIFEST_DIR"), "runner", redactor)
    .map_err(|e| RunnerError::Config(format!("startup init: {e}")))
}

/// Initialize tracing for subcommands that do not run jobs (masker discarded).
fn init_runner_tracing() -> Result<(), RunnerError> {
  init_tracing_for(&Arc::new(std::sync::Mutex::new(SecretMasker::new())))
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

async fn cmd_run(args: RunArgs) -> Result<(), Box<dyn std::error::Error>> {
  let masker = Arc::new(std::sync::Mutex::new(SecretMasker::new()));
  init_tracing_for(&masker).map_err(|e| format!("startup init: {e}"))?;

  let config_path = resolve_config(args.config)?;
  let cfg = load_run_config(&config_path)?;
  let data_dir = resolve_data_dir(&cfg.runtime.data_dir).map_err(|e| format!("{e}"))?;
  let workspace_root = resolve_work_dir(&cfg.runtime.work_dir);
  let lock_path = data_dir.join(".lock");

  // Acquire the single-job file lock — second `run` reads the body,
  // prints the PID, and exits 2. Release on graceful shutdown.
  let _lock_guard = lockfile::acquire(&lock_path, &config_path).map_err(|e| format!("{e}"))?;
  tracing::info!(path = %lock_path.display(), "acquired single-job lock");
  let runner_cfg = shared::RunnerConfig {
    data_dir,
    workspace_root,
    cgroup_path: None,
    services_mode: cfg.services_mode(),
    service_bind: cfg.service_bind(),
    cache: cfg.cache_config(),
    workspace_gc_hours: cfg.workspace_gc_hours(),
    shadow_enabled: cfg.shadow_enabled(),
  };

  let jit_config_b64 = require_jit_config(&cfg)?;
  let listener = GitHubListener::new(&jit_config_b64, runner_cfg, masker)
    .map_err(|e| format!("listener init: {e}"))?;

  let cancel = CancellationToken::new();
  spawn_signal_bridge(cancel.clone());
  // `--once` needs no special wiring: the JIT session is single-use, so
  // the listener exits after the first job completes. (An earlier stub
  // cancelled after 100ms here, which killed the poll loop before any
  // job could arrive.)
  if args.once {
    tracing::info!("--once is currently the default: the listener exits after the first job");
  }

  let result = listener
    .run(cancel)
    .await
    .map_err(|e| format!("listener: {e}"));
  // _lock_guard drops here, releasing the lock.
  result?;
  Ok(())
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
  config_path: &Path,
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

async fn cmd_remove(args: RemoveArgs) -> Result<(), Box<dyn std::error::Error>> {
  let masker = Arc::new(std::sync::Mutex::new(SecretMasker::new()));
  let redactor: Arc<dyn shared::startup::SecretRedactor> =
    Arc::new(MaskerRedactor(Arc::clone(&masker)));
  startup::init_with_redactor(env!("CARGO_MANIFEST_DIR"), "runner", redactor)
    .map_err(|e| format!("startup init: {e}"))?;

  let config_path = resolve_config(args.config)?;
  let creds_path = credentials_path_for(&config_path);
  if !config_path.exists() {
    println!("no registration found.");
    return Ok(());
  }
  let cfg = load_reg_config(&config_path).map_err(|e| format!("{e}"))?;
  let _ = args.token; // registration token reserved for live GH call in step 10

  let data_dir = resolve_data_dir(&cfg.runtime.data_dir).map_err(|e| format!("{e}"))?;
  let pending = data_dir.join(".pending_remove");
  let lock_path = data_dir.join(".lock");

  // Mid-job: refuse and write the pending marker unless --force. The
  // actual unregistration is wired in step 10; for now we just write
  // the marker and surface a clear message.
  if lock_path.exists() {
    if args.force {
      tracing::warn!("force-cancelling in-flight run (stub — live cancellation lands in step 10)");
    } else {
      let body = std::fs::read_to_string(&lock_path).unwrap_or_default();
      std::fs::write(&pending, body)?;
      return Err(format!(
        "another run is in flight; wrote {} marker. Re-run with --force to cancel, or wait for the current job to finish.",
        pending.display()
      )
      .into());
    }
  }

  // No active run — delete the persisted state. The live `acquire_job`
  // unregistration call lands in step 10.
  delete_registration_state(&config_path, &creds_path, &pending, &lock_path)?;
  println!(
    "unregistered runner '{}' (config, credentials, and lock removed; _diag kept). Live GH unregistration call lands in step 10.",
    cfg.runner_name
  );
  Ok(())
}

/// Delete a registration's persisted state: `config.toml`,
/// `credentials.json`, any `.pending_remove` marker, and the `.lock` file
/// (best-effort past the config itself). `_diag/` (logs + job journal) is
/// deliberately kept for `watch` history, and empty parent dirs stay in
/// place.
fn delete_registration_state(
  config_path: &Path,
  creds_path: &Path,
  pending: &Path,
  lock_path: &Path,
) -> Result<(), std::io::Error> {
  std::fs::remove_file(config_path)?;
  std::fs::remove_file(creds_path).ok();
  std::fs::remove_file(pending).ok();
  std::fs::remove_file(lock_path).ok();
  Ok(())
}
