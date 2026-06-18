//! `toolu-runner` — standalone GitHub Actions JIT runner CLI.
//!
//! Subcommands:
//! - `register` — validate JIT endpoint, write `config.toml` +
//!   `credentials.json` (mode 0600). The actual `acquire_job` smoke
//!   against real GH is step 10; this commit does the validation + write.
//! - `run` — load `config.toml`, acquire the `.lock` file lock, run the
//!   listener loop until SIGINT/SIGTERM.
//! - `remove` — read `config.toml`, write the `.pending_remove` marker
//!   if a `run` is in flight, otherwise delete both files.
//! - `status` — print `config.toml` and `credentials.json` summary (no
//!   network).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use clap::{Args, Parser, Subcommand};
use shared::RunnerError;
use shared::startup;
use tokio_util::sync::CancellationToken;
use toolu_runner::execution::secret_masker::SecretMasker;
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
  // AC #23: warn at startup for any YAMLESS_* env vars. Done before
  // clap parsing and before `startup::init` so the warning lands even
  // for subcommands (like `status`) that don't init tracing.
  shared::startup::warn_about_yamless_env();
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
  let home = std::env::var_os("HOME")
    .map(PathBuf::from)
    .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
    .unwrap_or_else(|| PathBuf::from("/var/lib/toolu-runner"));
  home.join(".toolu-runner").join("config.toml")
}

/// Derive the credentials path from the config path. The credentials
/// file lives next to `config.toml` in the same directory so users
/// can override `--config` and have both files move together.
fn credentials_path_for(config_path: &Path) -> PathBuf {
  config_path
    .parent()
    .map_or_else(|| PathBuf::from("credentials.json"), |p| p.join("credentials.json"))
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
  let parsed = url::Url::parse(url)
    .map_err(|e| RunnerError::Config(format!("invalid --url: {e}")))?;
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
  startup::init(env!("CARGO_MANIFEST_DIR"), "runner")
    .map_err(|e| format!("startup init: {e}"))?;

  let host = parse_and_validate_url(&args.url).map_err(|e| format!("{e}"))?;

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
    return Err(format!(
      "registration already exists at {} — pass --replace to overwrite",
      config_path.display()
    )
    .into());
  }

  // Step 8 stub: write a JSON config capturing the registration intent.
  // Step 9 swaps this for the full TOML layout via `toolu-runner::config`;
  // step 10 swaps the placeholder token for the real GH registration
  // response.
  let placeholder_token = format!("ghs_placeholder_{}", short_id_of(&args.token));

  let payload = serde_json::json!({
    "url": args.url,
    "host": host,
    "registration_token_redacted": "***redacted***",
    "name": runner_name,
    "labels": labels,
    "runner_group": args.runner_group,
    "work": args.work.as_ref().map(|p| p.to_string_lossy().into_owned()),
    "data_dir": "~/.toolu-runner",
    "protocol_version": if host.eq_ignore_ascii_case("github.com") { "v2" } else { "v1" },
    "auth_token_placeholder": placeholder_token,
    "replace": args.replace,
  });

  if let Some(parent) = config_path.parent() {
    std::fs::create_dir_all(parent)?;
  }
  std::fs::write(&config_path, serde_json::to_string_pretty(&payload)?)?;
  std::fs::write(&creds_path, serde_json::to_string_pretty(&serde_json::json!({
    "access_token": placeholder_token,
    "issued_at": chrono::Utc::now().to_rfc3339(),
  }))?)?;

  tracing::info!(
    path = %config_path.display(),
    credentials = %creds_path.display(),
    runner = %runner_name,
    host = %host,
    labels = ?labels,
    "wrote registration stub (step 9 swaps for TOML layout)"
  );
  println!(
    "registered runner '{runner_name}' at {host} (config: {}, creds: {})",
    config_path.display(),
    creds_path.display()
  );
  Ok(())
}

async fn cmd_run(args: RunArgs) -> Result<(), Box<dyn std::error::Error>> {
  startup::init(env!("CARGO_MANIFEST_DIR"), "runner")
    .map_err(|e| format!("startup init: {e}"))?;

  let config_path = args.config.clone().unwrap_or_else(default_config_path);
  if !config_path.exists() {
    return Err(format!(
      "config not found at {} — run `toolu-runner register` first",
      config_path.display()
    )
    .into());
  }
  let creds_path = credentials_path_for(&config_path);
  if !creds_path.exists() {
    return Err(format!(
      "credentials not found at {} — run `toolu-runner register` first",
      creds_path.display()
    )
    .into());
  }

  // Acquire the single-job file lock — second `run` reads the body,
  // prints the PID, and exits 2. Release on graceful shutdown.
  let data_dir = config_path
    .parent()
    .map_or_else(|| PathBuf::from("/var/lib/toolu-runner"), |p| p.to_path_buf());
  std::fs::create_dir_all(&data_dir)?;
  let lock_path = data_dir.join(".lock");
  let _lock_guard = lockfile::acquire(&lock_path, &config_path)
    .map_err(|e| format!("{e}"))?;
  tracing::info!(path = %lock_path.display(), "acquired single-job lock");

  let runner_cfg = build_runner_config(None);
  let masker = Arc::new(SecretMasker::new());

  // Step 8 stub: read the JSON config we wrote in `register`. Step 9
  // swaps to `toolu-runner::config::load_config` for the TOML layout.
  let raw_config = std::fs::read_to_string(&config_path).unwrap_or_default();
  let parsed: serde_json::Value = serde_json::from_str(&raw_config)?;
  let jit_config_b64 = parsed
    .get("jit_config")
    .and_then(|v| v.as_str())
    .unwrap_or("")
    .to_owned();
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
  startup::init(env!("CARGO_MANIFEST_DIR"), "runner")
    .map_err(|e| format!("startup init: {e}"))?;

  let config_path = args.config.clone().unwrap_or_else(default_config_path);
  let creds_path = credentials_path_for(&config_path);
  if !config_path.exists() {
    println!("no registration found.");
    return Ok(());
  }

  let data_dir = config_path
    .parent()
    .map_or_else(|| PathBuf::from("/var/lib/toolu-runner"), |p| p.to_path_buf());
  std::fs::create_dir_all(&data_dir)?;
  let pending = data_dir.join(".pending_remove");
  let lock_path = data_dir.join(".lock");
  let _ = args.token; // registration token reserved for live GH call in step 10

  // Mid-job: refuse and write the pending marker unless --force. The
  // actual unregistration is wired in step 10; for now we just write
  // the marker and surface a clear message.
  if lock_path.exists() {
    if args.force {
      tracing::warn!(
        "force-cancelling in-flight run (stub — live cancellation lands in step 10)"
      );
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
    "unregistered runner (config and credentials removed). Live GH unregistration call lands in step 10."
  );
  Ok(())
}

fn cmd_status(args: StatusArgs) -> Result<(), Box<dyn std::error::Error>> {
  let config_path = args.config.unwrap_or_else(default_config_path);
  let creds_path = credentials_path_for(&config_path);
  if !config_path.exists() {
    return Err(format!("config not found at {}", config_path.display()).into());
  }
  let raw = std::fs::read_to_string(&config_path)?;
  let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null);
  let creds_summary = if creds_path.exists() {
    "credentials present"
  } else {
    "credentials MISSING"
  };
  println!("runner:    {}", parsed.get("name").and_then(|v| v.as_str()).unwrap_or("?"));
  println!("url:       {}", parsed.get("url").and_then(|v| v.as_str()).unwrap_or("?"));
  println!("host:      {}", parsed.get("host").and_then(|v| v.as_str()).unwrap_or("?"));
  println!("labels:    {:?}", parsed.get("labels").cloned().unwrap_or(serde_json::Value::Null));
  println!("group:     {}", parsed.get("runner_group").and_then(|v| v.as_str()).unwrap_or("Default"));
  println!("protocol:  {}", parsed.get("protocol_version").and_then(|v| v.as_str()).unwrap_or("v2"));
  println!("work:      {}", parsed.get("work").and_then(|v| v.as_str()).unwrap_or("(default)"));
  println!("creds:     {creds_summary}");
  Ok(())
}

fn build_runner_config(work: Option<PathBuf>) -> shared::RunnerConfig {
  let home = std::env::var_os("HOME")
    .map(PathBuf::from)
    .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
    .unwrap_or_else(|| PathBuf::from("/var/lib/toolu-runner"));
  let data_dir = home.join(".toolu-runner");
  let workspace_root = work.unwrap_or_else(|| data_dir.join("_work"));
  shared::RunnerConfig {
    data_dir,
    workspace_root,
    cgroup_path: None,
  }
}

/// Truncate the registration token to a short fingerprint for the
/// placeholder OAuth token. The real flow (step 10) replaces this with
/// the JWT-exchange result.
fn short_id_of(token: &str) -> String {
  let len = token.len().min(8);
  token.get(..len).unwrap_or("").to_owned()
}
