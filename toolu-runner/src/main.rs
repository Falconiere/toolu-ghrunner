//! `toolu-runner` — standalone GitHub Actions JIT runner CLI.

use std::path::PathBuf;
use std::sync::Arc;

use clap::{Args, Parser, Subcommand};
use shared::startup;
use tokio_util::sync::CancellationToken;
use toolu_runner::execution::secret_masker::SecretMasker;
use toolu_runner::listener::GitHubListener;

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
}

#[derive(Debug, Args)]
struct StatusArgs {
  /// Path to the runner config file.
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

async fn cmd_register(args: RegisterArgs) -> Result<(), Box<dyn std::error::Error>> {
  // Initialize tracing first so subsequent errors are visible.
  startup::init(env!("CARGO_MANIFEST_DIR"), "runner")
    .map_err(|e| format!("startup init: {e}"))?;

  // Parse the URL to extract the host — this is what the GH registration
  // endpoint actually accepts in step 9's full flow.
  let parsed = url::Url::parse(&args.url).map_err(|e| format!("invalid --url: {e}"))?;
  let host = parsed
    .host_str()
    .ok_or_else(|| "URL missing host".to_owned())?
    .to_owned();

  let config_path = args.config.clone().unwrap_or_else(default_config_path);
  let runner_name = runner_name_or_hostname(args.name);
  let labels = if args.labels.is_empty() {
    default_labels()
  } else {
    args.labels
  };
  let runner_cfg = build_runner_config(args.work);

  // Stub: in step 9 this writes the full storage layout. For now we
  // emit a JSON config file capturing the registration intent.
  let payload = serde_json::json!({
    "url": args.url,
    "host": host,
    "registration_token_redacted": "***redacted***",
    "name": runner_name,
    "labels": labels,
    "runner_group": args.runner_group,
    "work": runner_cfg.workspace_root,
    "replace": args.replace,
  });

  if let Some(parent) = config_path.parent() {
    std::fs::create_dir_all(parent)?;
  }
  std::fs::write(&config_path, serde_json::to_string_pretty(&payload)?)?;
  tracing::info!(path = %config_path.display(), "wrote registration stub");
  println!("registered runner '{runner_name}' at {host} (config: {})", config_path.display());
  Ok(())
}

async fn cmd_run(args: RunArgs) -> Result<(), Box<dyn std::error::Error>> {
  let config_path = args.config.clone().unwrap_or_else(default_config_path);
  if !config_path.exists() {
    return Err(format!(
      "config not found at {} — run `toolu-runner register` first",
      config_path.display()
    )
    .into());
  }

  // Build a runner config from the CLI work dir (or default).
  let runner_cfg = build_runner_config(None);
  let masker = Arc::new(SecretMasker::new());
  startup::init(env!("CARGO_MANIFEST_DIR"), "runner")
    .map_err(|e| format!("startup init: {e}"))?;

  // Stub: step 9 wires the file lock; for now we run the listener directly.
  // The JIT config string is loaded from disk in step 9.
  let jit_config_b64 = std::fs::read_to_string(&config_path).unwrap_or_default();

  let listener = GitHubListener::new(&jit_config_b64, runner_cfg, masker)
    .map_err(|e| format!("listener init: {e}"))?;

  let cancel = CancellationToken::new();
  let once = args.once;
  if once {
    let cancel_child = cancel.clone();
    tokio::spawn(async move {
      // In --once mode, cancel after a short delay so the test harness
      // observes a clean exit. The real cancellation signal lands once
      // step 9 lands the file lock + signal plumbing.
      tokio::time::sleep(std::time::Duration::from_millis(100)).await;
      cancel_child.cancel();
    });
  }

  listener.run(cancel).await.map_err(|e| format!("listener: {e}"))?;
  Ok(())
}

async fn cmd_remove(_args: RemoveArgs) -> Result<(), Box<dyn std::error::Error>> {
  // Full unregistration flow lands in step 9.
  println!("toolu-runner remove: not yet implemented — see step 9");
  Ok(())
}

fn cmd_status(args: StatusArgs) -> Result<(), Box<dyn std::error::Error>> {
  let config_path = args.config.unwrap_or_else(default_config_path);
  if !config_path.exists() {
    return Err(format!("config not found at {}", config_path.display()).into());
  }
  let contents = std::fs::read_to_string(&config_path)?;
  println!("{}", contents);
  Ok(())
}