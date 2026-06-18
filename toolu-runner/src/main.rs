//! `toolu-runner` — standalone GitHub Actions JIT runner CLI.
//!
//! Subcommands:
//!
//! - `register` — validate URL + probe JIT endpoint, write `config.toml`
//!   and `credentials.json` (mode 0600). The full live registration
//!   flow (POST to JIT endpoint + JWT exchange) is step 10; this commit
//!   does the validation and write.
//! - `run` — load `config.toml`, acquire the `.lock` file lock, run the
//!   listener loop until SIGINT/SIGTERM.
//! - `remove` — read `config.toml`, write the `.pending_remove` marker
//!   if a `run` is in flight, otherwise delete both files. The live GH
//!   unregistration call lands in step 10.
//! - `status` — print `config.toml` and `credentials.json` summary
//!   (no network).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use clap::{Args, Parser, Subcommand};
use shared::RunnerError;
use shared::startup;
use tokio_util::sync::CancellationToken;
use toolu_runner::config::{
  CredentialsFile, RunnerRegistrationConfig, RuntimeConfig, jit_endpoint_for_host,
  load_config as load_reg_config, load_credentials, resolve_data_dir, resolve_work_dir,
  save_config as save_reg_config, save_credentials,
};
use toolu_runner::execution::secret_masker::{MaskerRedactor, SecretMasker};
use toolu_runner::listener::GitHubListener;
use toolu_runner::lockfile;

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
}

#[derive(Debug, Args)]
struct RegisterArgs {
  /// Repository or organization URL (e.g. https://github.com/owner/repo).
  #[arg(long)]
  url: String,
  /// Short-lived registration token from GitHub.
  #[arg(long)]
  token: String,
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
  /// Exit after the first job completes (test mode).
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

#[tokio::main]
async fn main() {
  // AC #23: warn at startup for any deprecated YAMLESS_* env vars. Done
  // before clap parsing and before `startup::init` so the warning lands
  // even for subcommands (like `status`) that don't init tracing.
  shared::startup::warn_about_legacy_env();
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
    Command::Status(args) => cmd_status(args),
  }
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

/// Validate that the JIT endpoint for `host` is reachable.
///
/// Uses a 5-second HEAD request. Returns `Ok(())` on any 2xx/3xx status;
/// errors with the response status on 4xx/5xx. Network errors propagate.
async fn probe_jit_endpoint(host: &str) -> Result<(), RunnerError> {
  let endpoint = jit_endpoint_for_host(host);
  let client = reqwest::Client::builder()
    .timeout(Duration::from_secs(5))
    .build()
    .map_err(|e| RunnerError::Network(format!("HTTP client: {e}")))?;
  let resp = client
    .head(&endpoint)
    .send()
    .await
    .map_err(|e| RunnerError::Network(format!("JIT endpoint probe failed: {e}")))?;
  let status = resp.status();
  if status.is_success() || status.is_redirection() {
    tracing::info!(endpoint = %endpoint, status = %status, "JIT endpoint reachable");
    Ok(())
  } else {
    Err(RunnerError::Network(format!(
      "JIT endpoint {endpoint} returned status {status}"
    )))
  }
}

async fn cmd_register(args: RegisterArgs) -> Result<(), Box<dyn std::error::Error>> {
  let masker = Arc::new(std::sync::Mutex::new(SecretMasker::new()));
  let redactor: Arc<dyn shared::startup::SecretRedactor> =
    Arc::new(MaskerRedactor(Arc::clone(&masker)));
  startup::init_with_redactor(env!("CARGO_MANIFEST_DIR"), "runner", redactor)
    .map_err(|e| format!("startup init: {e}"))?;

  let host = parse_and_validate_url(&args.url).map_err(|e| format!("{e}"))?;
  probe_jit_endpoint(&host)
    .await
    .map_err(|e| format!("{e}"))?;

  let config_path = args.config.clone().unwrap_or_else(default_config_path);
  let creds_path = credentials_path_for(&config_path);
  let runner_name = runner_name_or_hostname(args.name);
  let labels = if args.labels.is_empty() {
    default_labels()
  } else {
    args.labels
  };

  // Refuse if a registration already exists with the same name unless --replace.
  if config_path.exists() && !args.replace {
    return Err(
      format!(
        "registration already exists at {} — pass --replace to overwrite",
        config_path.display()
      )
      .into(),
    );
  }

  // Steps 3 & 4 (POST to JIT endpoint, exchange JWT for OAuth) are stubbed
  // here. The full live registration flow is exercised in step 10. We write
  // a placeholder auth_token + empty jit_config; `run` will refuse to
  // start until `register` is re-run with a real token.
  let placeholder_token = format!("ghs_placeholder_{}", short_id_of(&args.token));

  let runtime = RuntimeConfig {
    jit_config: String::new(),
    work_dir: args
      .work
      .as_ref()
      .map(|p| p.to_string_lossy().into_owned())
      .unwrap_or_else(|| "~/.toolu-runner/_work".to_owned()),
    data_dir: "~/.toolu-runner".to_owned(),
    protocol_version: if host.eq_ignore_ascii_case("github.com") {
      "v2".to_owned()
    } else {
      "v1".to_owned()
    },
  };

  let config = RunnerRegistrationConfig {
    runner_url: args.url.clone(),
    runner_name: runner_name.clone(),
    runner_id: 0,
    auth_token: placeholder_token.clone(),
    labels: labels.clone(),
    runner_group: args.runner_group.clone(),
    runtime,
  };

  save_reg_config(&config_path, &config).map_err(|e| format!("save config: {e}"))?;
  let creds = CredentialsFile {
    access_token: placeholder_token,
    issued_at: chrono::Utc::now().to_rfc3339(),
    expires_at: None,
  };
  save_credentials(&creds_path, &creds).map_err(|e| format!("save credentials: {e}"))?;

  tracing::info!(
    path = %config_path.display(),
    credentials = %creds_path.display(),
    runner = %runner_name,
    host = %host,
    labels = ?labels,
    "registered runner (stub — live flow is step 10)"
  );
  println!(
    "registered runner '{runner_name}' at {host} (config: {}, creds: {})",
    config_path.display(),
    creds_path.display()
  );
  Ok(())
}

async fn cmd_run(args: RunArgs) -> Result<(), Box<dyn std::error::Error>> {
  let masker = Arc::new(std::sync::Mutex::new(SecretMasker::new()));
  let redactor: Arc<dyn shared::startup::SecretRedactor> =
    Arc::new(MaskerRedactor(Arc::clone(&masker)));
  startup::init_with_redactor(env!("CARGO_MANIFEST_DIR"), "runner", redactor)
    .map_err(|e| format!("startup init: {e}"))?;

  let config_path = args.config.clone().unwrap_or_else(default_config_path);
  if !config_path.exists() {
    return Err(
      format!(
        "config not found at {} — run `toolu-runner register` first",
        config_path.display()
      )
      .into(),
    );
  }
  let creds_path = credentials_path_for(&config_path);
  if !creds_path.exists() {
    return Err(
      format!(
        "credentials not found at {} — run `toolu-runner register` first",
        creds_path.display()
      )
      .into(),
    );
  }

  let cfg = load_reg_config(&config_path).map_err(|e| format!("{e}"))?;
  let _creds = load_credentials(&creds_path).map_err(|e| format!("{e}"))?;

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
  };

  // The JIT config string is loaded from the persisted config. The
  // step-10 live flow will populate this with the real base64 blob;
  // until then `register` writes an empty string and `run` will exit
  // with a clear error from the listener constructor.
  let jit_config_b64 = cfg.runtime.jit_config.clone();
  if jit_config_b64.is_empty() {
    return Err(
      "config.toml has no JIT config blob — re-run `toolu-runner register` against a live GH repo (step 10 wires the live registration)".into(),
    );
  }

  let listener = GitHubListener::new(&jit_config_b64, runner_cfg, masker)
    .map_err(|e| format!("listener init: {e}"))?;

  let cancel = CancellationToken::new();
  // Bridge SIGINT/SIGTERM to the cancellation token.
  {
    let cancel_signal = cancel.clone();
    tokio::spawn(async move {
      let Ok(mut sigint) =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
      else {
        return;
      };
      let Ok(mut sigterm) =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
      else {
        return;
      };
      tokio::select! {
        _ = sigint.recv() => {},
        _ = sigterm.recv() => {},
      }
      cancel_signal.cancel();
    });
  }

  if args.once {
    let cancel_child = cancel.clone();
    tokio::spawn(async move {
      tokio::time::sleep(Duration::from_millis(100)).await;
      cancel_child.cancel();
    });
  }

  let result = listener
    .run(cancel)
    .await
    .map_err(|e| format!("listener: {e}"));
  // _lock_guard drops here, releasing the lock.
  result?;
  Ok(())
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

fn cmd_status(args: StatusArgs) -> Result<(), Box<dyn std::error::Error>> {
  let config_path = args.config.unwrap_or_else(default_config_path);
  let creds_path = credentials_path_for(&config_path);
  if !config_path.exists() {
    return Err(format!("config not found at {}", config_path.display()).into());
  }
  let cfg = load_reg_config(&config_path).map_err(|e| format!("{e}"))?;
  let creds_summary = if creds_path.exists() {
    "credentials present"
  } else {
    "credentials MISSING"
  };
  println!("runner:    {}", cfg.runner_name);
  println!("url:       {}", cfg.runner_url);
  println!("runner_id: {}", cfg.runner_id);
  println!("labels:    {:?}", cfg.labels);
  println!("group:     {}", cfg.runner_group);
  println!("protocol:  {}", cfg.runtime.protocol_version);
  println!("data_dir:  {}", cfg.runtime.data_dir);
  println!("work_dir:  {}", cfg.runtime.work_dir);
  println!("jit_cfg:   {} bytes", cfg.runtime.jit_config.len());
  println!("creds:     {creds_summary}");
  Ok(())
}

/// Truncate the registration token to a short fingerprint for the
/// placeholder OAuth token. The real flow (step 10) replaces this with
/// the JWT-exchange result.
fn short_id_of(token: &str) -> String {
  let len = token.len().min(8);
  token.get(..len).unwrap_or("").to_owned()
}
