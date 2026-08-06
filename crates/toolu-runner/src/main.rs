//! `toolu-runner` — standalone GitHub Actions JIT runner CLI.
//!
//! Subcommands: `register` (live `generate-jitconfig`, persists real
//! jit_config + runner_id), `run` (load config, hold `.lock`, run the
//! listener until SIGINT/SIGTERM), `remove` (delete state or write
//! `.pending_remove` mid-job), `status` (print config, no network),
//! `watch` (TUI over the job journal, no network).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use config::auth_store::{self, AuthStore};
use config::config::{RunnerRegistrationConfig, load_config as load_reg_config, resolve_data_dir};
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

/// Per-request timeout for `remove`'s GitHub unregister call. Matches the
/// register path's bound: long enough for a slow API, short enough that a
/// black-holed connection does not hang the CLI.
const UNREGISTER_TIMEOUT: Duration = Duration::from_secs(30);

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

  let data_dir = resolve_data_dir(&cfg.runtime.data_dir).map_err(|e| format!("{e}"))?;
  let pending = data_dir.join(".pending_remove");
  let lock_path = data_dir.join(".lock");

  let forced_past_run = refuse_if_run_in_flight(&lock_path, &pending, args.force)?;
  if forced_past_run {
    tracing::warn!(
      "--force with a run still in flight: skipping the GitHub unregister so the running job \
       can still renew and report. Remove the runner on GitHub once that job ends."
    );
  }

  // Unregister on GitHub BEFORE touching local state: the persisted
  // runner_id and URL are the only way to name the runner, so deleting them
  // first would strand it with no way to retry.
  let unregistered =
    unregister_on_github(&cfg, args.token, args.skip_unregister || forced_past_run).await?;

  delete_registration_state(&config_path, &creds_path, &pending, &lock_path)?;
  let gh = if unregistered {
    "unregistered on GitHub"
  } else {
    "still registered on GitHub — remove it there by hand"
  };
  println!(
    "removed runner '{}' ({gh}; config, credentials, and lock removed, _diag kept).",
    cfg.runner_name
  );
  Ok(())
}

/// Abort the removal when a run holds the job lock, writing the
/// `.pending_remove` marker the running process picks up between jobs.
/// `--force` proceeds instead (live cancellation is still step 10 work).
///
/// `Ok(true)` means `--force` bypassed a *held* lock, so a job is very
/// likely still running. The caller must not unregister in that case: the
/// job renews and reports against this registration, and deleting it on
/// GitHub mid-job would break the very run `--force` is aimed at.
fn refuse_if_run_in_flight(
  lock_path: &Path,
  pending: &Path,
  force: bool,
) -> Result<bool, Box<dyn std::error::Error>> {
  if !lock_path.exists() {
    return Ok(false);
  }
  if force {
    tracing::warn!("force-cancelling in-flight run (stub — live cancellation lands in step 10)");
    return Ok(true);
  }
  let body = std::fs::read_to_string(lock_path).unwrap_or_default();
  write_pending_marker(pending, &body)?;
  Err(format!(
    "another run is in flight; wrote {} marker. Re-run with --force to cancel, or wait for the current job to finish.",
    pending.display()
  )
  .into())
}

/// Unregister the runner on GitHub. `Ok(true)` when the call succeeded,
/// `Ok(false)` when it was deliberately skipped and local state should still
/// go — an explicit `--skip-unregister`, or no token to authenticate with.
///
/// A token that IS present and fails is an error: leaving a live runner
/// registered while deleting the only record of it is the bug this closes.
async fn unregister_on_github(
  cfg: &RunnerRegistrationConfig,
  flag: Option<String>,
  skip: bool,
) -> Result<bool, Box<dyn std::error::Error>> {
  if skip {
    tracing::warn!("--skip-unregister: leaving the runner registered on GitHub");
    return Ok(false);
  }
  let host = url::Url::parse(&cfg.runner_url)
    .ok()
    .and_then(|u| u.host_str().map(str::to_owned))
    .ok_or_else(|| format!("registration URL '{}' has no host", cfg.runner_url))?;
  let store = AuthStore::new(&registry::runner_home());
  let Some(token) = auth_store::resolve_bearer(&store, &host, flag)? else {
    tracing::warn!(
      "no GitHub token (--token / TOOLU_RUNNER_TOKEN / 'toolu-runner login') — \
       removing local state only; the runner stays registered on GitHub"
    );
    return Ok(false);
  };
  let client = reqwest::Client::new();
  wire::net::unregister_runner(
    &client,
    &cfg.runner_url,
    &token,
    cfg.runner_id,
    &cfg.runner_name,
    UNREGISTER_TIMEOUT,
  )
  .await
  .map_err(|e| {
    format!(
      "unregistering '{}' on GitHub failed: {e}\nNothing local was deleted — fix the token or \
       connectivity and retry, or pass --skip-unregister to remove local state anyway.",
      cfg.runner_name
    )
  })?;
  Ok(true)
}

/// Write the `.pending_remove` marker with owner-only perms (0600 on unix).
///
/// The body is the copied `.lock` JSON (holder PID, `started_at`,
/// `config_path`); it must not be world-readable, matching every other
/// runner state file. `mode(0o600)` only applies when `create` actually
/// creates the file, so an explicit `set_permissions` re-tightens a
/// pre-existing marker left at looser perms.
fn write_pending_marker(path: &Path, body: &str) -> std::io::Result<()> {
  #[cfg(unix)]
  {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let mut f = std::fs::OpenOptions::new()
      .write(true)
      .create(true)
      .truncate(true)
      .mode(0o600)
      .open(path)?;
    f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    f.write_all(body.as_bytes())
  }
  #[cfg(not(unix))]
  {
    std::fs::write(path, body)
  }
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
