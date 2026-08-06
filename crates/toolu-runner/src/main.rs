//! `toolu-runner` — standalone GitHub Actions JIT runner CLI.
//!
//! Subcommands: `register` (live `generate-jitconfig`, persists real
//! jit_config + runner_id), `run` (load config, hold `.lock`, run the
//! listener until SIGINT/SIGTERM), `boot` (env-driven, zero-config
//! one-shot mode for the container image — no persisted state), `remove`
//! (delete state or write `.pending_remove` mid-job), `status` (print
//! config, no network), `watch` (TUI over the job journal, no network).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use config::auth_store::{self, AuthStore};
use config::config::{RunnerRegistrationConfig, load_config as load_reg_config, resolve_data_dir};
use config::{lockfile, registry, repo_infer};
use shared::RunnerError;
use shared::startup;
use shared::{MaskerRedactor, SecretMasker};

mod boot_cmd;
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
  // `boot` needs richer exit codes than every other subcommand's uniform
  // Ok-is-0 / Err-is-2 (see `boot_cmd::cmd_boot`'s doc), so it is
  // intercepted here rather than folded into `run`'s `Result` mapping.
  if matches!(cli.command, Command::Boot(_)) {
    std::process::exit(boot_cmd::cmd_boot().await);
  }
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
    // Intercepted in `main` before `run` is called (see above).
    Command::Boot(_) => Ok(()),
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

  // Take the job lock rather than inspecting it. `acquire` is atomic, it
  // already implements this crate's stale-lock rule (dead holder AND old
  // enough), and HOLDING it for the whole removal closes the window where a
  // `run` could start between the check and the multi-second DELETE.
  let held = lockfile::acquire(&lock_path, &config_path);
  let skip = skip_reason(&held, args.skip_unregister);
  if let Err(RunnerError::LockHeld { pid, .. }) = &held
    && !args.force
  {
    // Best-effort, but not silent: the marker is more useful with the
    // holder's lock body in it, and an unreadable lock is itself worth
    // surfacing (permissions, a concurrent unlink).
    let body = std::fs::read_to_string(&lock_path).unwrap_or_else(|e| {
      tracing::warn!(error = %e, "could not read the job lock body — writing an empty marker");
      String::new()
    });
    write_pending_marker(&pending, &body)?;
    return Err(format!(
      "a run is in flight (pid {pid}); wrote {} marker. Re-run with --force to remove anyway, or wait for the current job to finish.",
      pending.display()
    )
    .into());
  }
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
  println!(
    "removed runner '{}' ({gh}; config, credentials{lock_note} removed, _diag kept).",
    cfg.runner_name
  );
  Ok(())
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
/// go — `--skip-unregister`, no token to authenticate with, or a URL this
/// repo-scoped endpoint cannot address (an org-level registration).
///
/// Only a RETRYABLE failure is an error: leaving a live runner registered
/// while deleting the only record of it is the bug this closes, so a bad
/// token or a dead network must not proceed. A `RunnerError::Config` is
/// different — the URL will never resolve, so failing would make `remove`
/// permanently impossible for org registrations, which worked before.
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
  // No token is an ERROR, not a warning-and-continue. Deleting the persisted
  // runner_id and URL while the runner is still registered is precisely the
  // B-002 outcome, and a WARN scrolls past; --skip-unregister makes that
  // choice explicit and reproduces the old behavior exactly.
  let Some(token) = remove_bearer(&cfg.runner_url, flag)? else {
    return Err(
      format!(
        "cannot unregister '{}' on GitHub: no token available.\nNothing local was deleted — \
       {NO_BEARER_HELP}.",
        cfg.runner_name
      )
      .into(),
    );
  };
  println!(
    "unregistering runner '{}' (id {}) from {} …",
    cfg.runner_name, cfg.runner_id, cfg.runner_url
  );
  let client = reqwest::Client::builder()
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

/// Decide whether a failed unregister should still clear local state.
///
/// `RunnerError::Config` from `unregister_runner` means the registration URL
/// cannot be turned into a repo-scoped runners endpoint at all — an org-level
/// URL, or one malformed enough that no host/owner/repo comes out. Either way
/// no retry can succeed, and refusing would make `remove` impossible for a
/// registration that removed fine before B-002. Everything else is
/// potentially transient, so it fails with local state intact.
fn classify_unregister_failure(
  name: &str,
  e: RunnerError,
) -> Result<bool, Box<dyn std::error::Error>> {
  if let RunnerError::Config(msg) = e {
    // Quote GitHub's/our own reason rather than asserting a cause: this
    // fires for any URL the runners API cannot address, not only org URLs.
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
  if !keep_lock {
    remove_best_effort(lock_path, "job lock");
  }
  Ok(())
}

/// Remove `path`, WARNing rather than failing if it cannot go.
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
