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
use config::config::{load_config as load_reg_config, resolve_data_dir};
use config::{registry, repo_infer};
use shared::RunnerError;
use shared::startup;
use shared::{MaskerRedactor, SecretMasker};

mod cli;
mod create_app_cmd;
mod login_cmd;
mod register_cmd;
mod run_cmd;
mod service_cmd;
mod setup_cmd;
mod status_cmd;
mod wizard_steps;

use crate::cli::{Cli, Command, RemoveArgs, WatchArgs, credentials_path_for, default_config_path};

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
    Command::Setup(args) => setup_cmd::cmd_setup(args).await,
    Command::Register(args) => register_cmd::cmd_register(args).await,
    Command::Run(args) => run_cmd::cmd_run(args).await,
    Command::Remove(args) => cmd_remove(args).await,
    Command::Status(args) => status_cmd::cmd_status(args),
    Command::Watch(args) => cmd_watch(args),
    Command::InstallService(args) => service_cmd::cmd_install_service(args),
    Command::Login(args) => login_cmd::cmd_login(args).await,
    Command::Logout(args) => login_cmd::cmd_logout(&args),
    Command::CreateApp(args) => {
      let home = registry::runner_home();
      create_app_cmd::cmd_create_app(&args, &home).await
    },
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
/// `<home>/config.toml` included). When [`registry::resolve_config_path`]
/// errors AND cwd inference did not apply, the error gains one
/// `cwd inference: …` clause saying why (non-github.com origin host, not
/// a git repo, no `origin` remote, unparseable remote) — so a GHES-origin
/// user sees why their remote never inferred.
fn resolve_config(flag: Option<PathBuf>) -> Result<PathBuf, Box<dyn std::error::Error>> {
  // Inference (and its git shell-out) only runs when no `--config` flag is
  // given: a flag invocation must work even where cwd inference cannot run.
  let (inferred, inference_note) = if flag.is_none() {
    classify_inference(repo_infer::detect_repo(&std::env::current_dir()?))
  } else {
    (None, None)
  };
  let owner_repo = inferred
    .as_ref()
    .map(|repo| (repo.owner.as_str(), repo.repo.as_str()));
  match registry::resolve_config_path(flag, &registry::runner_home(), owner_repo) {
    Ok(path) => Ok(path),
    Err(err) => match inference_note {
      Some(note) => Err(format!("{err} (cwd inference: {note})").into()),
      None => Err(err.into()),
    },
  }
}

/// Split a `detect_repo` outcome into the usable github.com inference and
/// a "why inference did not apply" note for error enrichment: a
/// non-github.com origin (the GHES case) names the host; a detection
/// failure keeps its own message minus the error-kind prefix and the
/// `pass --url` hint (`--url` is `register`-only — `run` / `status` /
/// `remove` have no such flag).
fn classify_inference(
  outcome: Result<repo_infer::InferredRepo, RunnerError>,
) -> (Option<repo_infer::InferredRepo>, Option<String>) {
  match outcome {
    Ok(repo) if repo.host.eq_ignore_ascii_case("github.com") => (Some(repo), None),
    Ok(repo) => (
      None,
      Some(format!("origin host '{}' is not github.com", repo.host)),
    ),
    Err(err) => {
      let msg = err.to_string();
      let msg = msg.strip_prefix("config error: ").unwrap_or(&msg);
      let msg = msg.split("; pass --url").next().unwrap_or(msg);
      (None, Some(msg.to_owned()))
    },
  }
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
