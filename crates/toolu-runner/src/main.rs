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

use clap::{Args, Parser, Subcommand};
use shared::RunnerError;
use shared::startup;
use shared::{MaskerRedactor, SecretMasker};
use tokio_util::sync::CancellationToken;
use config::auth_store::{self, AuthStore};
use config::config::{
  CacheSection, CredentialsFile, RunnerRegistrationConfig, RuntimeConfig, ServicesSection,
  ShadowSection, WorkspaceSection, load_config as load_reg_config, load_credentials,
  resolve_data_dir, resolve_work_dir, save_config as save_reg_config, save_credentials,
};
use listener::GitHubListener;
use config::lockfile;

mod login_cmd;
mod status_cmd;

/// github.com OAuth App `client_id` for the device-flow `login`.
/// Placeholder until the real App is registered.
const DEVICE_CLIENT_ID: &str = "REPLACE_ME";

/// Standalone GitHub Actions JIT runner.
#[derive(Debug, Parser)]
#[command(name = "toolu-runner", version, about, long_about = None)]
struct Cli {
  #[command(subcommand)]
  command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
  /// Register the runner with a GitHub repository or organization.
  Register(RegisterArgs),
  /// Run the listener loop, polling for jobs.
  Run(RunArgs),
  /// Remove the runner registration.
  Remove(RemoveArgs),
  /// Print local config and credential state (no network).
  Status(StatusArgs),
  /// Watch jobs in a TUI: history, live steps and logs, cancel key.
  Watch(WatchArgs),
  /// Log in to GitHub via the OAuth device flow and store the token.
  Login(login_cmd::LoginArgs),
  /// Delete the stored login token for a host.
  Logout(login_cmd::LogoutArgs),
}

#[derive(Debug, Args)]
struct RegisterArgs {
  /// Repository or organization URL (e.g. https://github.com/owner/repo).
  #[arg(long)]
  url: String,
  /// GitHub API token with repo admin rights (PAT or App installation
  /// token) for the `generate-jitconfig` REST call. Optional — resolution
  /// order is this flag > `TOOLU_RUNNER_TOKEN` env > the stored `login`
  /// token for the URL's host.
  #[arg(long)]
  token: Option<String>,
  /// Runner name (defaults to the hostname).
  #[arg(long)]
  name: Option<String>,
  /// Comma-separated labels (defaults to self-hosted,<os>,<arch>).
  #[arg(long, value_delimiter = ',')]
  labels: Vec<String>,
  /// Runner group (defaults to "Default").
  #[arg(long, default_value = "Default")]
  runner_group: String,
  /// Working directory for job workspaces.
  #[arg(long)]
  work: Option<PathBuf>,
  /// Path to the runner config file.
  #[arg(long)]
  config: Option<PathBuf>,
  /// Replace an existing registration with the same name.
  #[arg(long)]
  replace: bool,
}

#[derive(Debug, Args)]
struct RunArgs {
  /// Path to the runner config file.
  #[arg(long)]
  config: Option<PathBuf>,
  /// Exit after the first job completes. Currently a no-op: a JIT
  /// registration is single-use, so the listener always exits after one
  /// job with or without this flag. Kept for scripts and a future
  /// daemon mode, where omitting it would mean "keep listening".
  #[arg(long)]
  once: bool,
}

#[derive(Debug, Args)]
struct RemoveArgs {
  /// Path to the runner config file.
  #[arg(long)]
  config: Option<PathBuf>,
  /// Unregistration token (falls back to the registration token in config).
  #[arg(long)]
  token: Option<String>,
  /// Force-cancel an in-flight job before unregistering.
  #[arg(long)]
  force: bool,
}

#[derive(Debug, Args)]
struct StatusArgs {
  /// Path to the runner config file.
  #[arg(long)]
  config: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct WatchArgs {
  /// Path to the runner config file (default `~/.toolu-runner/config.toml`).
  /// When the file is absent or unreadable, `watch` falls back to browsing
  /// the default data dir (`~/.toolu-runner`) read-only — the fallback is in
  /// `watch::run_watch`.
  #[arg(long)]
  config: Option<PathBuf>,
}

#[tokio::main]
async fn main() {
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
    Command::Register(args) => cmd_register(args).await,
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
fn cmd_watch(args: WatchArgs) -> Result<(), Box<dyn std::error::Error>> {
  let config_path = args.config.unwrap_or_else(default_config_path);
  observability::watch::run_watch(&config_path)?;
  Ok(())
}

fn default_config_path() -> PathBuf {
  shared::paths::expand_tilde(Path::new("~/.toolu-runner/config.toml"))
}

/// Derive the credentials path from the config path. The credentials
/// file lives next to `config.toml` in the same directory so users
/// can override `--config` and have both files move together.
fn credentials_path_for(config_path: &Path) -> PathBuf {
  config_path.parent().map_or_else(
    || PathBuf::from("credentials.json"),
    |p| p.join("credentials.json"),
  )
}

fn runner_name_or_hostname(name: Option<String>) -> String {
  name.unwrap_or_else(|| {
    hostname::get()
      .ok()
      .and_then(|h| h.into_string().ok())
      .unwrap_or_else(|| "toolu-runner".to_owned())
  })
}

fn default_labels() -> Vec<String> {
  vec![
    "self-hosted".to_owned(),
    std::env::consts::OS.to_owned(),
    std::env::consts::ARCH.to_owned(),
  ]
}

fn parse_and_validate_url(url: &str) -> Result<String, RunnerError> {
  let parsed =
    url::Url::parse(url).map_err(|e| RunnerError::Config(format!("invalid --url: {e}")))?;
  let host = parsed
    .host_str()
    .ok_or_else(|| RunnerError::Config("URL missing host".to_owned()))?
    .to_owned();
  if !host.contains('.') {
    return Err(RunnerError::Config(format!(
      "invalid host '{host}' — runner accepts github.com and GHES hosts only"
    )));
  }
  Ok(host)
}

async fn cmd_register(args: RegisterArgs) -> Result<(), Box<dyn std::error::Error>> {
  init_runner_tracing().map_err(|e| format!("startup init: {e}"))?;

  let host = parse_and_validate_url(&args.url).map_err(|e| format!("{e}"))?;

  let config_path = args.config.clone().unwrap_or_else(default_config_path);
  let creds_path = credentials_path_for(&config_path);
  let runner_name = runner_name_or_hostname(args.name);
  let labels = if args.labels.is_empty() {
    default_labels()
  } else {
    args.labels
  };

  ensure_not_registered(&config_path, args.replace)?;

  // Resolve the REST bearer: --token flag > TOOLU_RUNNER_TOKEN env >
  // the stored `login` token for the URL's host. The token store lives
  // next to config.toml (no RunnerConfig is loaded during register).
  let data_dir = login_cmd::data_dir_for_config(&config_path);
  let token = auth_store::resolve_bearer(&AuthStore::new(&data_dir), &host, args.token.clone())?
    .ok_or_else(|| {
      RunnerError::Auth("no GitHub token: run 'toolu-runner login' or pass --token".to_owned())
    })?;

  let runner_id = register_and_persist(RegisterPersist {
    url: &args.url,
    token: &token,
    runner_name: &runner_name,
    labels: &labels,
    runner_group: &args.runner_group,
    work_folder: &work_folder_or_default(args.work.as_ref()),
    host: &host,
    config_path: &config_path,
    creds_path: &creds_path,
    replace: args.replace,
  })
  .await
  .map_err(|e| format!("{e}"))?;

  report_registered(
    &runner_name,
    runner_id,
    &host,
    &config_path,
    &creds_path,
    &labels,
  );
  Ok(())
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

/// Refuse to overwrite an existing registration unless `--replace` was given.
fn ensure_not_registered(
  config_path: &Path,
  replace: bool,
) -> Result<(), Box<dyn std::error::Error>> {
  if config_path.exists() && !replace {
    return Err(
      format!(
        "registration already exists at {} — pass --replace to overwrite",
        config_path.display()
      )
      .into(),
    );
  }
  Ok(())
}

/// Work-folder string from `--work`, defaulting to `~/.toolu-runner/_work`.
fn work_folder_or_default(work: Option<&PathBuf>) -> String {
  work
    .map(|p| p.to_string_lossy().into_owned())
    .unwrap_or_else(|| "~/.toolu-runner/_work".to_owned())
}

/// Log + print the registration result.
fn report_registered(
  runner_name: &str,
  runner_id: i64,
  host: &str,
  config_path: &Path,
  creds_path: &Path,
  labels: &[String],
) {
  tracing::info!(
    path = %config_path.display(),
    credentials = %creds_path.display(),
    runner = %runner_name,
    runner_id,
    host = %host,
    labels = ?labels,
    "registered runner via generate-jitconfig"
  );
  println!(
    "registered runner '{runner_name}' (id {runner_id}) at {host} (config: {}, creds: {})",
    config_path.display(),
    creds_path.display()
  );
}

/// Inputs for [`register_and_persist`] — the live register + write step.
struct RegisterPersist<'a> {
  url: &'a str,
  token: &'a str,
  runner_name: &'a str,
  labels: &'a [String],
  runner_group: &'a str,
  work_folder: &'a str,
  host: &'a str,
  config_path: &'a Path,
  creds_path: &'a Path,
  replace: bool,
}

/// POST `generate-jitconfig` for `p` and return the minted registration.
async fn mint_jit(
  p: &RegisterPersist<'_>,
) -> Result<wire::net::JitRegistration, RunnerError> {
  let client = reqwest::Client::builder()
    .timeout(Duration::from_secs(30))
    .build()
    .map_err(|e| RunnerError::Network(format!("HTTP client: {e}")))?;
  wire::net::register_jit(
    &client,
    &wire::net::RegisterParams {
      url: p.url,
      runner_token: p.token,
      name: p.runner_name,
      labels: p.labels,
      runner_group_id: runner_group_id(p.runner_group),
      work_folder: p.work_folder,
      replace: p.replace,
    },
  )
  .await
}

/// Live JIT registration (all-or-nothing): POST generate-jitconfig, parse
/// the minted config, then persist real config + credentials. Returns the
/// assigned runner ID. Any failure returns before touching either file.
///
/// The RSA→JWT→OAuth2 chain runs at `run` time from the stored jit_config,
/// not here. `auth_token` stores the runner's non-secret `client_id`.
async fn register_and_persist(p: RegisterPersist<'_>) -> Result<i64, RunnerError> {
  let registration = mint_jit(&p).await?;

  // Decode the minted config to confirm it parses and to lift the
  // client_id (a stable, non-secret identity) for the auth_token field.
  let parsed = protocol::JitConfig::parse(&registration.encoded_jit_config)
    .map_err(|e| RunnerError::Protocol(format!("minted jit_config did not parse: {e}")))?;
  let client_id = parsed.credentials.data.client_id;
  let runner_id = registration.runner_id;

  let config = build_registration_config(&p, &client_id, registration);

  // Snapshot any pre-existing config BEFORE overwriting so a rollback can
  // restore it — re-registration must not destroy the previous registration
  // when the credentials write fails.
  let previous_config = std::fs::read(p.config_path).ok();

  // Persist only after the live call + parse both succeed.
  save_reg_config(p.config_path, &config)?;
  let creds = CredentialsFile {
    access_token: client_id,
    issued_at: chrono::Utc::now().to_rfc3339(),
    expires_at: None,
  };
  // Registration is all-or-nothing: a credentials write failure must not
  // leave a config without creds (a half-registered state). Roll the config
  // file back (best-effort) before surfacing the error.
  if let Err(e) = save_credentials(p.creds_path, &creds) {
    roll_back_config(p.config_path, previous_config.as_deref());
    return Err(e);
  }
  Ok(runner_id)
}

/// Best-effort rollback of the config file after a failed registration:
/// restore the pre-existing bytes when there were any (the overwrite keeps
/// the file's 0600 mode), otherwise remove the newly created file.
fn roll_back_config(path: &std::path::Path, previous: Option<&[u8]>) {
  let result = match previous {
    Some(bytes) => std::fs::write(path, bytes),
    None => std::fs::remove_file(path),
  };
  if let Err(e) = result {
    tracing::warn!(error = %e, "failed to roll back config after credentials write error");
  }
}

/// Assemble the persisted [`RunnerRegistrationConfig`] from the minted
/// registration. `auth_token` carries the non-secret `client_id`.
fn build_registration_config(
  p: &RegisterPersist<'_>,
  client_id: &str,
  registration: wire::net::JitRegistration,
) -> RunnerRegistrationConfig {
  let runtime = RuntimeConfig {
    jit_config: registration.encoded_jit_config,
    work_dir: p.work_folder.to_owned(),
    data_dir: "~/.toolu-runner".to_owned(),
    protocol_version: if p.host.eq_ignore_ascii_case("github.com") {
      "v2".to_owned()
    } else {
      "v1".to_owned()
    },
  };
  RunnerRegistrationConfig {
    runner_url: p.url.to_owned(),
    runner_name: p.runner_name.to_owned(),
    runner_id: registration.runner_id,
    auth_token: client_id.to_owned(),
    labels: p.labels.to_vec(),
    runner_group: p.runner_group.to_owned(),
    runtime,
    services: ServicesSection::default(),
    cache: CacheSection::default(),
    workspace: WorkspaceSection::default(),
    shadow: ShadowSection::default(),
  }
}

/// Map a `--runner-group` string to a `generate-jitconfig` group ID.
///
/// A numeric value is used directly; non-numeric (e.g. a group name)
/// yields `None`, which [`net::register_jit`] defaults to `1` (Default).
/// A non-empty, non-numeric value (a group *name*) is not supported by the
/// JIT API and is WARNed about so the fallback to Default is not silent.
fn runner_group_id(group: &str) -> Option<i64> {
  let trimmed = group.trim();
  if let Ok(id) = trimmed.parse::<i64>() {
    return Some(id);
  }
  if !trimmed.is_empty() {
    tracing::warn!(
      runner_group = trimmed,
      "runner group names are not supported (a numeric group ID is required); \
       defaulting to the Default group"
    );
  }
  None
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

  let config_path = args.config.clone().unwrap_or_else(default_config_path);
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

  let config_path = args.config.clone().unwrap_or_else(default_config_path);
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
  std::fs::remove_file(&config_path)?;
  std::fs::remove_file(&creds_path).ok();
  std::fs::remove_file(&pending).ok();
  println!(
    "unregistered runner '{}' (config and credentials removed). Live GH unregistration call lands in step 10.",
    cfg.runner_name
  );
  Ok(())
}
