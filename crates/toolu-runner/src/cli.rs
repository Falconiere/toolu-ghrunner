//! clap CLI surface for `toolu-runner`: the [`Cli`] parser, the
//! [`Command`] subcommand enum, per-subcommand argument structs, and the
//! arg-default helpers they document.
//!
//! Split out of `main.rs` to keep the CLI entrypoint under the crate's
//! per-file complexity limit. `login` / `logout` args live in
//! `login_cmd.rs` with their handlers.

use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand, ValueHint};

use crate::login_cmd;

/// Extended help footer for the top-level `--help`.
const TOP_AFTER_HELP: &str = "\
Examples:
  toolu-runner login                # one-time GitHub OAuth device-flow login
  toolu-runner register             # repo inferred from the cwd git remote
  toolu-runner run                  # poll for the job, execute it, exit
  toolu-runner watch                # TUI over running and past jobs

Environment:
  TOOLU_RUNNER_TOKEN           GitHub bearer for `register` (flag > env > stored login token)
  TOOLU_RUNNER_CLIENT_ID       OAuth App client_id for `login` (fallback for --client-id)
  TOOLU_RUNNER_HOME            runner state root (default ~/.toolu-runner)
  TOOLU_RUNNER_LOG / RUST_LOG  tracing filter; levels above `info` also require
                               TOOLU_RUNNER_ALLOW_VERBOSE=1 (secret-leak guard)";

/// Extended help footer for `create-app --help`.
const CREATE_APP_AFTER_HELP: &str = "\
Examples:
  # create the runner's GitHub App on github.com (opens a browser)
  toolu-runner create-app

  # custom name, no browser (print the URL to open manually)
  toolu-runner create-app --name my-org-runner --no-browser";

/// Extended help footer for `register --help`.
const REGISTER_AFTER_HELP: &str = "\
Examples:
  # zero-arg: repo inferred from the cwd git remote `origin` (github.com)
  cd my-repo && toolu-runner register

  # explicit github.com repository, bearer from the stored `login` token
  toolu-runner register --url https://github.com/owner/repo

  # GHES organization, explicit token, custom name + labels
  toolu-runner register --url https://ghes.example.com/my-org \\
    --token <PAT> --name build-01 --labels self-hosted,linux,arm64";

#[derive(Debug, Parser)]
#[command(
  name = "toolu-runner",
  version,
  propagate_version = true,
  arg_required_else_help = true,
  after_help = TOP_AFTER_HELP
)]
/// Standalone GitHub Actions JIT runner.
///
/// Registers a just-in-time runner against a GitHub repository or
/// organization and executes one workflow job per registration — a JIT
/// config is single-use, so the flow is `login` once, then `register` +
/// `run` per job. No orchestrator service; all state lives under
/// `~/.toolu-runner`.
pub(crate) struct Cli {
  #[command(subcommand)]
  pub(crate) command: Command,
}

/// Top-level subcommands.
#[derive(Debug, Subcommand)]
pub(crate) enum Command {
  /// Register the runner with a GitHub repository or organization.
  ///
  /// POSTs GitHub's `generate-jitconfig` REST endpoint and persists the
  /// minted runner config and credentials locally (all-or-nothing). The
  /// registration is single-use (JIT): the first `run` consumes it —
  /// re-register for each job.
  Register(RegisterArgs),
  /// Run the listener loop, polling for jobs.
  ///
  /// Loads the persisted registration, acquires the exclusive single-job
  /// lock (`~/.toolu-runner/.lock`), creates a broker session, and polls
  /// until a job arrives. Executes the job, streams logs to GitHub, then
  /// exits — a JIT registration is single-use. SIGINT / SIGTERM cancel
  /// gracefully.
  Run(RunArgs),
  /// Remove the runner registration.
  ///
  /// Deletes the persisted config and credentials. If a run is in flight
  /// (the job lock is held) it refuses and writes a `.pending_remove`
  /// marker instead — pass --force to cancel the run first. The live
  /// GitHub unregister call is not yet wired.
  Remove(RemoveArgs),
  /// Print local config and credential state (no network).
  ///
  /// Shows the persisted registration, whether the credentials file is
  /// present, and the stored device-flow login state for the registered
  /// host. Reads local files only.
  Status(StatusArgs),
  /// Watch jobs in a TUI: history, live steps and logs, cancel key.
  ///
  /// Read-only terminal UI over the local job journal
  /// (`data_dir/_diag/jobs`). No network: it replays and tails journal
  /// files, including when no runner is registered. Cancelling a job
  /// sends SIGINT to the lock-holding `run` process (unix only).
  Watch(WatchArgs),
  /// Log in to GitHub via the OAuth device flow and store the token.
  ///
  /// Prints a one-time code, opens the verification page, polls until
  /// the grant completes, then stores the token in the OS keyring (0600
  /// file fallback). `register` uses this token when neither --token nor
  /// TOOLU_RUNNER_TOKEN is set.
  Login(login_cmd::LoginArgs),
  /// Delete the stored login token for a host.
  ///
  /// Idempotent: a missing token is a no-op.
  Logout(login_cmd::LogoutArgs),
  /// Create the runner's GitHub App via the App-manifest flow.
  ///
  /// Opens a browser to GitHub's App-manifest page, captures the minted
  /// App's credentials on a loopback callback, and saves them (0600) to
  /// `<home>/github-app.json` — an account-level identity shared by every
  /// repo. github.com only this release. Re-run with --force to replace a
  /// previously created App.
  CreateApp(CreateAppArgs),
}

/// Arguments for the `create-app` subcommand.
#[derive(Debug, Args)]
#[command(after_help = CREATE_APP_AFTER_HELP)]
pub(crate) struct CreateAppArgs {
  /// GitHub App name (default: toolu-runner-<hostname>).
  ///
  /// Must be unique across all of GitHub — a duplicate name makes GitHub
  /// refuse the redirect, which surfaces here as a browser-flow timeout.
  #[arg(long, value_name = "NAME")]
  pub(crate) name: Option<String>,
  /// GitHub host to create the App on (github.com only this release).
  ///
  /// GHES and organization-owned Apps are not supported yet; any other
  /// value is rejected before any browser launch or network call.
  #[arg(long, default_value = "github.com", value_name = "HOST", value_hint = ValueHint::Hostname)]
  pub(crate) host: String,
  /// Print the URL to open manually instead of launching a browser.
  #[arg(long)]
  pub(crate) no_browser: bool,
  /// Overwrite an existing saved GitHub App (`<home>/github-app.json`).
  ///
  /// Without it, `create-app` refuses (before any network call) when an
  /// App is already saved under the runner home.
  #[arg(long)]
  pub(crate) force: bool,
}

/// Arguments for the `register` subcommand.
#[derive(Debug, Args)]
#[command(after_help = REGISTER_AFTER_HELP)]
pub(crate) struct RegisterArgs {
  /// Repository or organization URL, e.g. https://github.com/owner/repo.
  ///
  /// Optional: when absent, the repository is inferred from the cwd git
  /// remote `origin` (github.com only). Org registrations and GHES hosts
  /// still require --url. A repository URL registers a repo-level runner;
  /// an organization URL an org-level one.
  #[arg(long, value_name = "URL", value_hint = ValueHint::Url)]
  pub(crate) url: Option<String>,
  /// GitHub API token for the `generate-jitconfig` REST call.
  ///
  /// Needs admin rights on the target repo/org (PAT or App installation
  /// token). Optional — resolution order: this flag > TOOLU_RUNNER_TOKEN
  /// env > the stored `login` token for the URL's host. Used only during
  /// registration, never at job runtime.
  #[arg(long, value_name = "TOKEN")]
  pub(crate) token: Option<String>,
  /// Runner name (default: the machine hostname).
  #[arg(long, value_name = "NAME")]
  pub(crate) name: Option<String>,
  /// Comma-separated runner labels (default: self-hosted,<os>,<arch>).
  #[arg(long, value_name = "LABELS", value_delimiter = ',')]
  pub(crate) labels: Vec<String>,
  /// Numeric runner group ID for org registrations (default: the Default
  /// group).
  ///
  /// Group *names* are not supported by the JIT API: any non-numeric value
  /// other than "Default" logs a warning and falls back to the Default
  /// group (ID 1).
  #[arg(long, default_value = "Default", value_name = "ID")]
  pub(crate) runner_group: String,
  /// Working directory for job workspaces (default: ~/.toolu-runner/_work).
  #[arg(long, value_name = "DIR", value_hint = ValueHint::DirPath)]
  pub(crate) work: Option<PathBuf>,
  /// Path to the runner config file. Default: inferred from the cwd git
  /// remote (runners/<owner>/<repo>/ under the runner home), else the
  /// sole existing registration.
  ///
  /// The credentials file is written next to it in the same directory.
  #[arg(long, value_name = "FILE", value_hint = ValueHint::FilePath)]
  pub(crate) config: Option<PathBuf>,
  /// Replace an existing registration with the same name.
  ///
  /// Without it, `register` refuses to overwrite an existing config.
  #[arg(long)]
  pub(crate) replace: bool,
}

/// Arguments for the `run` subcommand.
#[derive(Debug, Args)]
pub(crate) struct RunArgs {
  /// Path to the runner config file. Default: inferred from the cwd git
  /// remote (runners/<owner>/<repo>/ under the runner home), else the
  /// sole existing registration.
  #[arg(long, value_name = "FILE", value_hint = ValueHint::FilePath)]
  pub(crate) config: Option<PathBuf>,
  /// Exit after the first job completes (currently the default behavior).
  ///
  /// A JIT registration is single-use, so the listener always exits
  /// after one job with or without this flag. Kept for scripts and a
  /// future daemon mode, where omitting it would mean "keep listening".
  #[arg(long)]
  pub(crate) once: bool,
}

/// Arguments for the `remove` subcommand.
#[derive(Debug, Args)]
pub(crate) struct RemoveArgs {
  /// Path to the runner config file. Default: inferred from the cwd git
  /// remote (runners/<owner>/<repo>/ under the runner home), else the
  /// sole existing registration.
  #[arg(long, value_name = "FILE", value_hint = ValueHint::FilePath)]
  pub(crate) config: Option<PathBuf>,
  /// Unregistration token (reserved).
  ///
  /// The live GitHub unregister call is not yet wired; when it lands,
  /// this falls back to the registration token in config.
  #[arg(long, value_name = "TOKEN")]
  pub(crate) token: Option<String>,
  /// Cancel an in-flight run before removing state.
  ///
  /// Without it, a held job lock aborts the removal and writes a
  /// `.pending_remove` marker for the running process.
  #[arg(long)]
  pub(crate) force: bool,
}

/// Arguments for the `status` subcommand.
#[derive(Debug, Args)]
pub(crate) struct StatusArgs {
  /// Path to the runner config file. Default: inferred from the cwd git
  /// remote (runners/<owner>/<repo>/ under the runner home), else the
  /// sole existing registration.
  #[arg(long, value_name = "FILE", value_hint = ValueHint::FilePath)]
  pub(crate) config: Option<PathBuf>,
}

/// Arguments for the `watch` subcommand.
#[derive(Debug, Args)]
pub(crate) struct WatchArgs {
  /// Path to the runner config file. Default: inferred from the cwd git
  /// remote (runners/<owner>/<repo>/ under the runner home), else the
  /// sole existing registration.
  ///
  /// When the file is absent or unreadable, `watch` falls back to
  /// browsing the default data dir (~/.toolu-runner) read-only.
  #[arg(long, value_name = "FILE", value_hint = ValueHint::FilePath)]
  pub(crate) config: Option<PathBuf>,
}

/// Default `--config` path: `~/.toolu-runner/config.toml`.
pub(crate) fn default_config_path() -> PathBuf {
  shared::paths::expand_tilde(Path::new("~/.toolu-runner/config.toml"))
}

/// Runner name from `--name`, defaulting to the machine hostname.
pub(crate) fn runner_name_or_hostname(name: Option<String>) -> String {
  name.unwrap_or_else(|| {
    hostname::get()
      .ok()
      .and_then(|h| h.into_string().ok())
      .unwrap_or_else(|| "toolu-runner".to_owned())
  })
}

/// Default `--labels`: `self-hosted,<os>,<arch>`.
pub(crate) fn default_labels() -> Vec<String> {
  vec![
    "self-hosted".to_owned(),
    std::env::consts::OS.to_owned(),
    std::env::consts::ARCH.to_owned(),
  ]
}

/// Derive the credentials path from the config path. The credentials
/// file lives next to `config.toml` in the same directory so users
/// can override `--config` and have both files move together.
pub(crate) fn credentials_path_for(config_path: &Path) -> PathBuf {
  config_path.parent().map_or_else(
    || PathBuf::from("credentials.json"),
    |p| p.join("credentials.json"),
  )
}

/// Work-folder string from `--work`, defaulting to `~/.toolu-runner/_work`.
pub(crate) fn work_folder_or_default(work: Option<&PathBuf>) -> String {
  work
    .map(|p| p.to_string_lossy().into_owned())
    .unwrap_or_else(|| "~/.toolu-runner/_work".to_owned())
}

/// clap's own consistency check, run at startup in debug builds only:
/// panics on conflicting or invalid argument definitions. A bin-only
/// crate has no lib target for a unit test to import [`Cli`] from, so
/// the integration tests exercise this through `cargo run` (debug
/// profile) instead.
#[cfg(debug_assertions)]
pub(crate) fn debug_assert_cli() {
  use clap::CommandFactory;
  Cli::command().debug_assert();
}
