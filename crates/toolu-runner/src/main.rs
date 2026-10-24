//! `toolu-runner` — standalone GitHub Actions JIT runner CLI.
//!
//! Subcommands: `register` (live `generate-jitconfig`, persists real
//! `jit_config` + `runner_id`), `run` (load config, hold `.lock`, run the
//! listener until SIGINT/SIGTERM; `run --all` supervises every
//! registration in one process), `boot` (env-driven, zero-config
//! one-shot mode for the container image — no persisted state), `remove`
//! (delete state or write `.pending_remove` mid-job), `status` (print
//! config, no network), `check` (read-only connectivity / tools / proxy
//! preflight).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ::config::auth_store::{self, AuthStore};
use ::config::config::{
  RunnerRegistrationConfig, load_config as load_reg_config, resolve_data_dir,
};
use ::config::{lockfile, registry, repo_infer};
use clap::Parser;
use shared::RunnerError;
use shared::startup;
use shared::{MaskerRedactor, SecretMasker};

mod app_auth;
mod app_import;
mod app_install;
mod boot_cmd;
mod check_cmd;
mod check_endpoints;
mod check_env;
mod check_probe;
mod check_report;
mod check_tools;
mod cli;
mod config;
mod create_app_cmd;
mod doctor_auth;
mod doctor_cmd;
mod doctor_host;
mod doctor_registrations;
mod doctor_report;
mod doctor_service;
mod ephemeral;
mod login_cmd;
mod machine_service;
mod network_setup;
mod poll_job;
mod poll_loop;
mod register_auth;
mod register_cmd;
mod run_cmd;
mod service_cmd;
mod status_cmd;
mod supervisor;
mod supervisor_signal;

use crate::cli::{Cli, Command, RemoveArgs, credentials_path_for};

#[tokio::main]
async fn main() {
  #[cfg(debug_assertions)]
  cli::debug_assert_cli();
  let cli = Cli::parse();
  // `boot` needs richer exit codes than every other subcommand's uniform
  // Ok-is-0 / Err-is-2 (see `boot_cmd::cmd_boot`'s doc), so it is
  // intercepted here rather than folded into `run`'s `Result` mapping.
  if matches!(cli.command, Command::Boot(_)) {
    std::process::exit(boot_cmd::cmd_boot().await);
  }
  // `check` and `doctor` likewise own their exit codes (0 / 1 / 2 usage).
  let cli = match cli {
    Cli {
      command: Command::Check(args),
    } => std::process::exit(check_cmd::cmd_check(args).await),
    Cli {
      command: Command::Doctor(args),
    } => std::process::exit(doctor_cmd::cmd_doctor(args).await),
    other => other,
  };
  let exit_code = match Box::pin(run(cli)).await {
    Ok(()) => 0,
    Err(err) => {
      eprintln!("toolu-runner: {err}");
      err
        .downcast_ref::<run_cmd::CodedExit>()
        .map_or(2, |coded| coded.code)
    },
  };
  std::process::exit(exit_code);
}

async fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
  match cli.command {
    Command::Register(args) => register_cmd::cmd_register(args).await,
    Command::Run(args) if args.all => Box::pin(supervisor::cmd_run_all()).await,
    Command::Run(args) => Box::pin(run_cmd::cmd_run(args)).await,
    Command::Remove(args) => cmd_remove(args).await,
    Command::Status(args) => status_cmd::cmd_status(args),
    // Unit files, `systemctl` / `launchctl`: all blocking, so off the runtime.
    Command::InstallService(args) => tokio::task::spawn_blocking(move || {
      service_cmd::cmd_install_service(&args).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("install-service did not finish: {e}"))?
    .map_err(Into::into),
    Command::Login(args) => login_cmd::cmd_login(args).await,
    Command::Logout(args) => login_cmd::cmd_logout(&args),
    Command::CreateApp(args) => {
      let home = registry::runner_home();
      create_app_cmd::cmd_create_app(&args, &home).await
    },
    // Intercepted in `main` before `run` is called (see above).
    Command::Boot(_) | Command::Check(_) | Command::Doctor(_) => Ok(()),
  }
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

async fn cmd_remove(args: RemoveArgs) -> Result<(), Box<dyn std::error::Error>> {
  init_remove_tracing()?;

  let config_path = resolve_config(args.config)?;
  let creds_path = credentials_path_for(&config_path);
  if !config_path.exists() {
    println!("no registration found.");
    return Ok(());
  }
  let mut cfg = load_reg_config(&config_path).map_err(|e| format!("{e}"))?;

  let data_dir = resolve_data_dir(&cfg.runtime.data_dir).map_err(|e| format!("{e}"))?;
  let pending = data_dir.join(".pending_remove");
  let lock_path = data_dir.join(".lock");

  // Take the job lock rather than inspecting it. `acquire` is atomic, it
  // already implements this crate's stale-lock rule (dead holder AND old
  // enough), and HOLDING it for the whole removal closes the window where a
  // `run` could start between the check and the multi-second DELETE.
  let home = registry::runner_home();
  let mut held = lockfile::acquire(&lock_path, &config_path);
  if let Err(RunnerError::LockHeld { pid, .. }) = &held
    && !args.force
  {
    match drain_held_registration(
      &home,
      &config_path,
      &pending,
      &lock_path,
      *pid,
      &cfg.runner_name,
    )
    .await?
    {
      Some((reloaded, guard)) => {
        cfg = reloaded;
        held = Ok(guard);
      },
      None => return Ok(()),
    }
  }
  let skip = skip_reason(&held, args.skip_unregister);
  let guard = held.ok();

  // Unregister on GitHub BEFORE touching local state: the persisted
  // runner_id and URL are the only way to name the runner, so deleting them
  // first would strand it with no way to retry.
  let unregistered = unregister_on_github(&cfg, args.token, skip).await?;

  // A live run still owns `.lock`; deleting it would let a second `run`
  // acquire a fresh one against the same data_dir and race the running job.
  let keep_lock = guard.is_none();
  delete_registration_state(&config_path, &creds_path, &pending, &lock_path, keep_lock)?;
  drop(guard);
  let service = after_removal(&home, keep_lock)?;

  print_removal_summary(
    &cfg.runner_name,
    unregistered,
    keep_lock,
    service.as_deref(),
  );
  Ok(())
}

async fn drain_held_registration(
  home: &Path,
  config_path: &Path,
  pending: &Path,
  lock_path: &Path,
  pid: u32,
  old_name: &str,
) -> Result<Option<(RunnerRegistrationConfig, lockfile::LockGuard)>, Box<dyn std::error::Error>> {
  // Keep the holder's lock body in the marker when it can be read.
  let body = std::fs::read_to_string(lock_path).unwrap_or_else(|e| {
    tracing::warn!(error = %e, "could not read the job lock body — writing an empty marker");
    String::new()
  });
  write_pending_marker(pending, &body)?;
  let wait = crate::config::remove_wait();
  let guard = drained_lock(home, pid, lock_path, config_path, pending, wait).await?;
  // An ephemeral job may have removed itself; a re-mint may have changed the ID.
  if !config_path.exists() {
    already_removed(home, old_name, lock_path, pending, guard)?;
    return Ok(None);
  }
  let cfg = load_reg_config(config_path).map_err(|e| format!("{e}"))?;
  Ok(Some((cfg, guard)))
}

fn init_remove_tracing() -> Result<(), Box<dyn std::error::Error>> {
  let masker = Arc::new(std::sync::Mutex::new(SecretMasker::new()));
  let redactor: Arc<dyn shared::startup::SecretRedactor> =
    Arc::new(MaskerRedactor(Arc::clone(&masker)));
  startup::init_with_redactor(env!("CARGO_MANIFEST_DIR"), "runner", redactor)
    .map_err(|e| format!("startup init: {e}"))?;
  Ok(())
}

fn print_removal_summary(name: &str, unregistered: bool, keep_lock: bool, service: Option<&Path>) {
  let gh = if unregistered {
    "unregistered on GitHub"
  } else {
    "still registered on GitHub — remove it there by hand"
  };
  let lock_note = if keep_lock {
    ", job lock left for the running process"
  } else {
    " and lock"
  };
  println!("removed runner '{name}' ({gh}; config, credentials{lock_note} removed, _diag kept).");
  if let Some(unit) = service {
    println!(
      "removed the toolu-runner service ({}): no registrations remain",
      unit.display()
    );
  }
}

/// An ephemeral registration finished its job and removed itself while
/// `remove` waited: drop the lock and marker this removal created, and
/// uninstall the machine service if it was the last registration.
fn already_removed(
  home: &Path,
  name: &str,
  lock_path: &Path,
  pending: &Path,
  guard: lockfile::LockGuard,
) -> Result<(), Box<dyn std::error::Error>> {
  remove_best_effort(pending, "pending-remove marker");
  remove_best_effort(lock_path, "job lock");
  drop(guard);
  println!("runner '{name}' already removed itself (its one job ended).");
  if let Some(unit) = after_removal(home, false)? {
    println!(
      "removed the toolu-runner service ({}): no registrations remain",
      unit.display()
    );
  }
  Ok(())
}

/// The registration's lock once the `run --all` supervisor holding it
/// (`pid`) has drained its loop; an error naming what to do when the holder
/// is not the supervisor, or a job keeps the loop busy past `wait`.
async fn drained_lock(
  home: &Path,
  pid: u32,
  lock_path: &Path,
  config_path: &Path,
  pending: &Path,
  wait: Duration,
) -> Result<lockfile::LockGuard, Box<dyn std::error::Error>> {
  match supervisor_signal::drain_and_wait(home, pid, lock_path, config_path, wait).await {
    supervisor_signal::Drain::Released(guard) => Ok(guard),
    supervisor_signal::Drain::StillBusy(pid) => Err(
      format!(
        "the toolu-runner service (pid {pid}) is running a job for this runner and stops it \
         when the job ends ({} marker written). Re-run `toolu-runner remove` then, or pass \
         --force to remove anyway.",
        pending.display()
      )
      .into(),
    ),
    supervisor_signal::Drain::NotSupervised => Err(
      format!(
        "a run is in flight (pid {pid}); wrote {} marker. Re-run with --force to remove anyway, \
         or wait for the current job to finish.",
        pending.display()
      )
      .into(),
    ),
  }
}

/// After the state is gone: the last registration's removal also
/// deactivates and deletes the machine unit (#208); a `--force` removal
/// (a job still running) never does, and instead has the supervisor rescan
/// so it drains the vanished registration.
fn after_removal(
  home: &Path,
  keep_lock: bool,
) -> Result<Option<PathBuf>, Box<dyn std::error::Error>> {
  if keep_lock {
    if let Err(e) = supervisor_signal::notify(home) {
      tracing::warn!(error = %e, "could not notify the toolu-runner service");
    }
    return Ok(None);
  }
  machine_service::uninstall_if_last(home).map_err(|e| {
    format!(
      "the registration was removed, but removing the toolu-runner service failed: {e}; run \
       `toolu-runner install-service --remove`"
    )
    .into()
  })
}

/// Why `remove` will not call GitHub, if it will not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SkipReason {
  /// The operator passed `--skip-unregister`.
  Requested,
  /// A live run holds the job lock and `--force` bypassed it. That job
  /// renews and reports against this registration, so unregistering now
  /// would break the very run `--force` targets.
  RunInFlight,
}

/// Decide whether to skip the unregister, keeping the two reasons distinct
/// so the warning names the one that actually applies.
fn skip_reason(
  held: &Result<lockfile::LockGuard, RunnerError>,
  requested: bool,
) -> Option<SkipReason> {
  if requested {
    return Some(SkipReason::Requested);
  }
  matches!(held, Err(RunnerError::LockHeld { .. })).then_some(SkipReason::RunInFlight)
}

/// Remedies to name whenever `remove` cannot authenticate the unregister.
const NO_BEARER_HELP: &str = "run 'toolu-runner login', pass --token, or set TOOLU_RUNNER_TOKEN; \
   pass --skip-unregister to remove local state and leave the runner registered on GitHub";

/// Resolve the bearer for the unregister call, keyed by the registration
/// URL's host: `--token` > `TOOLU_RUNNER_TOKEN` > the stored `login` token,
/// the same precedence `register` uses. `Ok(None)` means no usable token.
///
/// An unreadable token store is `Ok(None)`, not an error: a corrupt
/// `token-<host>.json` should route to the same actionable "no token"
/// message as having none, rather than aborting `remove` with a parse error.
/// An empty token counts as absent — sending `Bearer ` only earns a 401.
fn remove_bearer(
  runner_url: &str,
  flag: Option<String>,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
  let parsed = url::Url::parse(runner_url)
    .map_err(|e| format!("registration URL '{runner_url}' is not a URL: {e}"))?;
  let host = parsed
    .host_str()
    .ok_or_else(|| format!("registration URL '{runner_url}' has no host"))?
    .to_owned();
  let store = AuthStore::new(&registry::runner_home());
  let resolved = auth_store::resolve_bearer(&store, &host, flag).unwrap_or_else(|e| {
    tracing::warn!(error = %e, "could not read the stored login token — treating it as absent");
    None
  });
  Ok(resolved.filter(|t| !t.trim().is_empty()))
}

/// Unregister the runner on GitHub. `Ok(true)` when the call succeeded,
/// `Ok(false)` when it was deliberately skipped and local state should still
/// go — `--skip-unregister`, no token to authenticate with, or a URL no
/// runners endpoint can address (it names no owner/repo or organization).
///
/// Only a RETRYABLE failure is an error: leaving a live runner registered
/// while deleting the only record of it is the bug this closes, so a bad
/// token or a dead network must not proceed. A `RunnerError::Config` is
/// different — the URL will never resolve, so failing would make `remove`
/// permanently impossible for such a registration.
async fn unregister_on_github(
  cfg: &RunnerRegistrationConfig,
  flag: Option<String>,
  skip: Option<SkipReason>,
) -> Result<bool, Box<dyn std::error::Error>> {
  match skip {
    Some(SkipReason::Requested) => {
      tracing::warn!("--skip-unregister: leaving the runner registered on GitHub");
      return Ok(false);
    },
    Some(SkipReason::RunInFlight) => {
      tracing::warn!(
        "--force with a run still in flight: skipping the GitHub unregister so the running job \
         can still renew and report. Remove the runner on GitHub once that job ends."
      );
      return Ok(false);
    },
    None => {},
  }
  let token = unregister_token(cfg, flag)?;
  println!(
    "unregistering runner '{}' (id {}) from {} …",
    cfg.runner_name, cfg.runner_id, cfg.runner_url
  );
  // The registration's `[network]` (#94): the unregister must use the same
  // proxy / CA as `run`.
  let client = network::client_builder(&cfg.network_config())?
    .build()
    .map_err(|e| format!("building the HTTP client for the unregister failed: {e}"))?;
  let outcome = wire::net::unregister_runner(
    &client,
    &cfg.runner_url,
    &token,
    cfg.runner_id,
    &cfg.runner_name,
    UNREGISTER_TIMEOUT,
  )
  .await;
  match outcome {
    Ok(()) => Ok(true),
    Err(e) => classify_unregister_failure(&cfg.runner_name, e),
  }
}

fn unregister_token(
  cfg: &RunnerRegistrationConfig,
  flag: Option<String>,
) -> Result<String, Box<dyn std::error::Error>> {
  // No token is an error: deleting the runner ID first would strand it.
  if let Some(token) = remove_bearer(&cfg.runner_url, flag)? {
    return Ok(token);
  }
  // `remove` does not mint App installation tokens yet (#201 follow-up).
  let app_note = if cfg.auth.is_token() {
    ""
  } else {
    " This registration re-mints with a GitHub App, but unregistering it still needs a \
     personal token."
  };
  Err(
    format!(
      "cannot unregister '{}' on GitHub: no token available.{app_note}\nNothing local was \
     deleted — {NO_BEARER_HELP}.",
      cfg.runner_name
    )
    .into(),
  )
}

/// Decide whether a failed unregister should still clear local state.
///
/// `RunnerError::Config` from `unregister_runner` means the registration URL
/// cannot be turned into a repo or org runners endpoint at all — it is
/// malformed enough that no host and owner/org comes out. No retry can
/// succeed, and refusing would make `remove` impossible for a registration
/// that removed fine before B-002. Everything else is potentially transient,
/// so it fails with local state intact.
fn classify_unregister_failure(
  name: &str,
  e: RunnerError,
) -> Result<bool, Box<dyn std::error::Error>> {
  if let RunnerError::Config(msg) = e {
    // Quote our own reason rather than asserting a cause: this fires for
    // any URL the runners API cannot address.
    tracing::warn!(
      "cannot address a runners API endpoint for '{name}' ({msg}) — removing local state only; \
       remove the runner on GitHub by hand"
    );
    return Ok(false);
  }
  Err(
    format!(
      "unregistering '{name}' on GitHub failed: {e}\nNothing local was deleted — fix the token \
       or connectivity and retry, or pass --skip-unregister to remove local state anyway."
    )
    .into(),
  )
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
///
/// `keep_lock` spares `.lock` when a live run still owns it. fs2's advisory
/// lock is inode-scoped and `LockGuard` has no `Drop` that unlinks the path,
/// so removing the file would leave the running process holding a lock on an
/// unlinked inode — a later `run` would create a fresh `.lock`, acquire it,
/// and execute a second job against the same `data_dir` and workspace.
fn delete_registration_state(
  config_path: &Path,
  creds_path: &Path,
  pending: &Path,
  lock_path: &Path,
  keep_lock: bool,
) -> Result<(), std::io::Error> {
  std::fs::remove_file(config_path)?;
  remove_best_effort(creds_path, "credentials");
  remove_best_effort(pending, "pending-remove marker");
  remove_best_effort(
    &pending.with_file_name(::config::stop_marker::FILE_NAME),
    "stop record",
  );
  if !keep_lock {
    remove_best_effort(lock_path, "job lock");
  }
  Ok(())
}

/// Remove `path`, `WARNing` rather than failing if it cannot go.
///
/// These are best-effort past `config.toml` — the registration is already
/// gone once that is deleted — but "best-effort" here means WARN-and-
/// continue, not silence: a leftover `.lock` in particular is what a later
/// `run` weighs in its stale-lock check, so an operator wondering why it
/// reports a job in flight deserves the reason in the log. A missing file
/// is the expected case and stays quiet.
fn remove_best_effort(path: &Path, what: &str) {
  match std::fs::remove_file(path) {
    Ok(()) => {},
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
    Err(e) => tracing::warn!(error = %e, path = %path.display(), "could not remove the {what}"),
  }
}
