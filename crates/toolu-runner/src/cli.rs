//! clap CLI surface for `toolu-runner`: the [`Cli`] parser, the
//! [`Command`] subcommand enum, per-subcommand argument structs, and the
//! arg-default helpers they document.
//!
//! Split out of `main.rs` to keep the CLI entrypoint under the crate's
//! per-file complexity limit. `login` / `logout` args live in
//! `login_cmd.rs` with their handlers.

use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand, ValueHint};

use crate::{check_cmd, doctor_cmd, login_cmd, setup};

/// Extended help footer for the top-level `--help`.
const TOP_AFTER_HELP: &str = "\
Examples:
  toolu-runner login                # one-time GitHub OAuth device-flow login
  toolu-runner register             # repo inferred from the cwd git remote
  toolu-runner run                  # stay online, re-minting after each job
  toolu-runner check                # preflight: GitHub reachability, tools, proxy/CA
  toolu-runner doctor               # health: what is broken and the command that fixes it

Environment:
  TOOLU_RUNNER_TOKEN           GitHub bearer: `register` (--token > env > saved App > login)
                               and a token registration's re-mint (env > login)
  TOOLU_RUNNER_CLIENT_ID       OAuth App client_id for `login` (fallback for --client-id)
  TOOLU_RUNNER_HOME            runner state root (default ~/.toolu-runner)
  TOOLU_RUNNER_ENROLL_TOKEN    toolu.sh enrollment token once `setup` chose toolu.sh (fallback for --enroll-token)
  TOOLU_RUNNER_TOOLU_API_URL   toolu.sh origin `setup` pairs with (default https://console.toolu.sh)
  TOOLU_RUNNER_KEYRING         opt in to the OS keyring token store (default: 0600 file)
  TOOLU_RUNNER_NO_KEYRING      force the file token store; overrides TOOLU_RUNNER_KEYRING
  TOOLU_RUNNER_NODE_DIR        offline Node.js runtimes (official runner externals/ layout);
                               never downloads when set
  TOOLU_RUNNER_LOG / RUST_LOG  tracing filter; levels above `info` also require
                               TOOLU_RUNNER_ALLOW_VERBOSE=1 (secret-leak guard)";

/// Extended help footer for `create-app --help`.
const CREATE_APP_AFTER_HELP: &str = "\
Examples:
  # an App for your personal repositories (opens a browser twice: create, then install)
  toolu-runner create-app --account octocat

  # an App owned by the organization acme, for org-level runners
  toolu-runner create-app --org acme

  # reuse an App another machine created (the key is checked with GitHub first)
  toolu-runner create-app --import /etc/toolu/github-app.json
  toolu-runner create-app --app-id 123456 --app-key ./toolu-runner.private-key.pem

After creating (or importing) the App, create-app waits for its installation:
o + Enter opens the install link again, c + Enter copies it, ctrl+c cancels
(exit 130). After 10 minutes a terminal asks Keep waiting / Open again /
Cancel; without one, create-app exits 1. The App is saved as soon as GitHub
returns it, so `create-app --import <home>/github-app.json` resumes the wait.

Over SSH, or with no display, create-app prints a get.toolu.sh link to open on
another machine and picks the code up from the relay (TOOLU_RUNNER_RELAY_URL)
with a secret only this machine holds; p + Enter pastes the code from the
callback page instead. With --no-relay this machine never contacts the relay:
paste the address-bar URL of the page that fails to load. --headless / --no-headless override the guess.";

/// Extended help footer for `boot --help`.
const BOOT_AFTER_HELP: &str = "\
Env:
  TOOLU_JITCONFIG   required — GitHub's encoded_jit_config from
                    POST /orgs/{org}/actions/runners/generate-jitconfig,
                    verbatim (the 3-blob envelope protocol::JitConfig::parse
                    handles). Missing or empty: exit 2, no network attempted.
  TOOLU_DEADLINE    optional epoch-milliseconds decimal string (/^\\d+$/), a
                    self-timeout backstop (e.g. now+6h). Missing or
                    unparseable: runs without a watchdog. Already past: exit
                    124 immediately, before any network call. Mid-job: the
                    watchdog cancels the job gracefully, then hard-exits 124
                    after a 30s grace period.

Examples:
  # container ENTRYPOINT — no other args or env needed
  TOOLU_JITCONFIG=<encoded> TOOLU_DEADLINE=<epoch_ms> toolu-runner boot

Exit codes:
  0    job completed Success or Skipped, or the runner shut down (signal or
       session end) before any job was acquired
  1    job completed Failure or Cancelled (non-deadline), or the listener
       errored (auth, network, protocol)
  2    environment/config error before polling (missing TOOLU_JITCONFIG,
       unparseable JIT envelope)
  5    session conflict outlasted the bounded retry
  124  the TOOLU_DEADLINE watchdog fired";

/// Extended help footer for `register --help`.
const REGISTER_AFTER_HELP: &str = "\
Examples:
  # zero-arg: repo inferred from the cwd git remote `origin` (github.com)
  cd my-repo && toolu-runner register

  # explicit github.com repository; bearer from the saved GitHub App
  # (`create-app`) or else the stored `login` token
  toolu-runner register --url https://github.com/owner/repo

  # organization runner through the GitHub App, group chosen by name
  toolu-runner register --app --url https://github.com/acme --runner-group ci

  # GHES organization, explicit token, custom name + extra labels
  # (registered as self-hosted,<OS>,<arch>,gpu)
  toolu-runner register --url https://ghes.example.com/my-org \\
    --token <PAT> --name build-01 --labels gpu

  # only the listed labels, no self-hosted/<OS>/<arch> defaults
  toolu-runner register --no-default-labels --labels build-farm,gpu

  # one job, then the runner unregisters and deletes this registration
  toolu-runner register --ephemeral --labels autoscaled";

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
/// organization. Each JIT config is single-use; `run` re-mints after each
/// job with the saved GitHub App or a token from the environment or `login`.
/// `setup` also supports pairing with toolu.sh. All state lives under
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
  /// JIT config is single-use; `run` re-mints after each job using the
  /// registration's GitHub App or a token from the environment or `login`.
  Register(RegisterArgs),
  /// Run the listener loop, staying online across jobs.
  ///
  /// Acquires the registration lock, polls for jobs, and re-mints a JIT
  /// config after each job. `--once` exits after one job; ephemeral runners
  /// unregister after theirs. `--all` supervises every registration under the
  /// runner home and rescans on SIGHUP. SIGINT / SIGTERM cancel gracefully.
  Run(RunArgs),
  /// Remove the runner registration.
  ///
  /// Unregisters the runner from GitHub, then deletes the persisted config
  /// and credentials. Takes the job lock to decide: if a run holds it, a
  /// `.pending_remove` marker is written; when the holder is the machine
  /// service (`run --all`) it is asked to stop just this runner and remove
  /// waits up to 30 s (a job in flight finishes first), otherwise the
  /// removal refuses — pass --force to remove anyway (which keeps the lock
  /// and skips the unregister). Removing the last registration also
  /// deactivates and deletes the machine service. A failed unregister, or no token at all, aborts before
  /// anything local is deleted so the removal can be retried;
  /// --skip-unregister removes local state only.
  Remove(RemoveArgs),
  /// Print local config and credential state (no network).
  ///
  /// Shows the persisted registration, whether the credentials file is
  /// present, and the stored device-flow login state for the registered
  /// host. Reads local files only.
  Status(StatusArgs),
  /// Install the service that keeps the runner alive across reboots.
  ///
  /// Without --config: the one machine service for every registration.
  /// With --config: a per-registration unit. Linux root uses a system unit; other users use a user
  /// unit. Non-root systemd installs ask to enable linger if needed;
  /// `--enable-linger` and `--yes` answer Yes. `--print` previews,
  /// `--no-activate` writes only, and `--remove` deactivates and deletes.
  InstallService(InstallServiceArgs),
  /// Log in to GitHub via the OAuth device flow and store the token.
  ///
  /// Prints a one-time code, opens the verification page, polls until
  /// the grant completes, then stores the token as a 0600 file under the
  /// runner home (or in the OS keyring with `TOOLU_RUNNER_KEYRING=1`).
  /// `register` uses this token when no explicit token or saved GitHub App
  /// is selected. Token registrations also use it for re-minting.
  Login(login_cmd::LoginArgs),
  /// Delete the stored login token for a host.
  ///
  /// Idempotent: a missing token is a no-op.
  Logout(login_cmd::LogoutArgs),
  /// Create (or import) the runner's GitHub App and wait for its installation.
  ///
  /// Opens a browser to GitHub's App-manifest page (personal, or `--org`
  /// for an org-owned App), captures the minted App's credentials on a
  /// loopback callback, saves them (0600) to `<home>/github-app.json` — an
  /// account-level identity shared by every machine and repo — then waits
  /// until the App is installed and records the installation. `--import` /
  /// `--app-id` reuse an existing App instead. github.com only this release.
  /// Re-run with --force to replace a previously saved App.
  CreateApp(CreateAppArgs),
  /// Zero-config one-shot mode for the toolu.sh container image.
  ///
  /// No flags: the image's ENTRYPOINT is `["toolu-runner", "boot"]` and
  /// providers pass only env vars (never argv — JIT config in argv would
  /// leak into provider dashboards). Reads `TOOLU_JITCONFIG` (required) and
  /// `TOOLU_DEADLINE` (optional watchdog backstop) straight from the
  /// container's environment, runs exactly one job with no persisted
  /// state (no config.toml, no credentials.json, no `.lock`, no re-mint),
  /// and exits with a code reflecting the job outcome. See `--help` for
  /// the full env contract and exit-code mapping.
  Boot(BootArgs),
  /// Preflight: can this host reach GitHub and run jobs? (read-only)
  ///
  /// Probes the API, codeload, pipelines, broker, token and results hosts
  /// the runner needs (the resolved registration's own endpoints when one
  /// exists, else `--url`, else github.com), each bounded by `--timeout`,
  /// and reports git / Docker / Node availability plus the effective proxy
  /// and TLS-trust settings with credentials redacted. Never takes the job
  /// lock, registers, or opens a session. Exit 0 = all required probes
  /// passed, 1 = a required probe failed, 2 = usage/config error. See
  /// `doctor` for the whole-machine health check.
  Check(check_cmd::CheckArgs),
  /// Health: is this machine serving its registrations, and how to fix it.
  ///
  /// Checks the install (version, newer release), the host (platform, uid,
  /// free disk) and every registration under the runner home, printing a
  /// fix command under each warning or failure that has one. Read-only: never takes the
  /// job lock, writes a file or registers. `--offline` makes no network
  /// request. Exit 0 = no failure, 1 = a failure, 2 = usage error.
  Doctor(doctor_cmd::DoctorArgs),
  /// Set up this machine as a runner for GitHub or toolu.sh.
  ///
  /// Check the machine, ask (or take flags) for the job source and credentials,
  /// review, register or pair, install the machine service, and confirm online.
  ///
  /// With `--yes` or no terminal, no question is asked: a missing answer
  /// exits 2 naming its flag and writes nothing. Re-running on a
  /// configured machine offers Keep as is / Add another runner /
  /// Reconfigure / Remove. Exit 0 ok, 1 runtime failure, 2 usage or
  /// auth, 130 cancelled.
  Setup(setup::args::SetupArgs),
}

/// Arguments for the `create-app` subcommand.
#[derive(Debug, Args)]
#[command(after_help = CREATE_APP_AFTER_HELP)]
pub(crate) struct CreateAppArgs {
  /// GitHub App name (default: toolu-runner-<org>, toolu-runner-<account>,
  /// else toolu-runner-<hostname>).
  ///
  /// Must be unique across all of GitHub — a duplicate name makes GitHub
  /// refuse the redirect, which surfaces here as a browser-flow timeout.
  /// GitHub's form lets you change it before creating the App.
  #[arg(long, value_name = "NAME")]
  pub(crate) name: Option<String>,
  /// Create the App owned by this organization, for org-level runners
  /// (permission: Self-hosted runners, write).
  ///
  /// The org is checked on GitHub before the browser opens. Only org owners
  /// (and its GitHub App managers) can create Apps for an org. With
  /// `--import`, the installation is looked for on this org.
  #[arg(long, value_name = "ORG", conflicts_with = "account")]
  pub(crate) org: Option<String>,
  /// Your personal account login: names the App toolu-runner-<LOGIN>
  /// (a personal App gets Administration, write, for repo runners).
  #[arg(long, value_name = "LOGIN")]
  pub(crate) account: Option<String>,
  /// Reuse an existing App from a github-app.json instead of creating one.
  ///
  /// The key is proven with a GitHub App JWT call before anything is saved.
  #[arg(
    long,
    value_name = "FILE",
    value_hint = ValueHint::FilePath,
    conflicts_with_all = ["app_id", "name", "account"]
  )]
  pub(crate) import: Option<PathBuf>,
  /// Reuse an existing App by id (with `--app-key`) instead of creating one.
  #[arg(
    long,
    value_name = "ID",
    requires = "app_key",
    conflicts_with_all = ["name", "account"]
  )]
  pub(crate) app_id: Option<i64>,
  /// The existing App's private-key PEM file (with `--app-id`).
  #[arg(long, value_name = "PEM_FILE", value_hint = ValueHint::FilePath, requires = "app_id")]
  pub(crate) app_key: Option<PathBuf>,
  /// GitHub host to create the App on (github.com only this release).
  ///
  /// GHES is not supported yet; any other value is rejected before any
  /// browser launch or network call.
  #[arg(long, default_value = "github.com", value_name = "HOST", value_hint = ValueHint::Hostname)]
  pub(crate) host: String,
  /// Print the URLs to open manually instead of launching a browser.
  #[arg(long)]
  pub(crate) no_browser: bool,
  /// Where the browser runs (`--headless` / `--no-headless` / `--no-relay`).
  #[command(flatten)]
  pub(crate) session: HeadlessFlags,
  /// The account the App must belong to when no `--org`/`--account` names
  /// it (`setup`'s repository owner): a returned App owned by anyone else is
  /// refused.
  #[arg(skip)]
  pub(crate) expected_owner: Option<String>,
  /// Overwrite an existing saved GitHub App (`<home>/github-app.json`).
  ///
  /// Without it, `create-app` refuses (before any network call) when an
  /// App is already saved under the runner home.
  #[arg(long)]
  pub(crate) force: bool,
}

/// How a GitHub App gets created relative to this machine's browser
/// (#205, design screen 06); shared by `create-app` and `setup`.
#[derive(Debug, Default, Clone, Copy, Args)]
pub(crate) struct HeadlessFlags {
  /// Create the App from another machine's browser (default over SSH or
  /// with no display): print a link and pick the code up from the relay.
  #[arg(long, conflicts_with = "no_headless")]
  pub(crate) headless: bool,
  /// Use this machine's browser and the loopback callback, even over SSH.
  #[arg(long, conflicts_with = "no_relay")]
  pub(crate) no_headless: bool,
  /// Headless without the get.toolu.sh relay: paste the address-bar URL of
  /// the page GitHub redirects to (implies --headless; needs a terminal).
  #[arg(long)]
  pub(crate) no_relay: bool,
}

impl HeadlessFlags {
  /// The flags' verdict, if they give one: `--headless` / `--no-relay` →
  /// headless, `--no-headless` → desktop, none → `None` (guess).
  pub(crate) fn forced(self) -> Option<bool> {
    if self.headless || self.no_relay {
      Some(true)
    } else if self.no_headless {
      Some(false)
    } else {
      None
    }
  }
}

/// Arguments for the `register` subcommand.
#[derive(Debug, Args)]
#[command(after_help = REGISTER_AFTER_HELP)]
pub(crate) struct RegisterArgs {
  /// Repository or organization URL, e.g. <https://github.com/owner/repo>.
  ///
  /// Optional: when absent, the repository is inferred from the cwd git
  /// remote `origin` (github.com only). Org registrations and GHES hosts
  /// still require --url. A repository URL registers a repo-level runner;
  /// an organization URL an org-level one.
  #[arg(long, value_name = "URL", value_hint = ValueHint::Url)]
  pub(crate) url: Option<String>,
  /// How `register` authenticates (`--token`, `--app`).
  #[command(flatten)]
  pub(crate) auth: RegisterAuthArgs,
  /// Runner name (default: the machine hostname).
  #[arg(long, value_name = "NAME")]
  pub(crate) name: Option<String>,
  /// Comma-separated custom labels, added to the defaults.
  ///
  /// The defaults are `self-hosted`, the OS (`Linux` / `macOS`) and the
  /// architecture (`X64` / `ARM64`) — GitHub's `RUNNER_OS` / `RUNNER_ARCH`
  /// spelling. Empty entries are ignored, labels differing only in case are
  /// kept once, and a label may be at most 256 characters (UTF-16 units, as
  /// upstream counts them). Required with
  /// --no-default-labels.
  #[arg(long, value_name = "LABELS", value_delimiter = ',')]
  pub(crate) labels: Vec<String>,
  /// Register only the --labels values, without the self-hosted / OS /
  /// architecture defaults.
  #[arg(long)]
  pub(crate) no_default_labels: bool,
  /// Make this an ephemeral registration: `run` takes exactly one job, then
  /// unregisters the runner on GitHub and deletes the local registration.
  ///
  /// Recorded in config.toml (`ephemeral = true`). `run` never re-mints for
  /// it; shutting down before a job keeps the registration for the next
  /// `run`. Not for `install-service`, whose unit restarts forever.
  #[arg(long)]
  pub(crate) ephemeral: bool,
  /// Runner group for org registrations: a name or a numeric ID (default:
  /// the Default group).
  ///
  /// For an organization URL a name is resolved through the org's runner
  /// group list and its ID is recorded. Repository runners have only the
  /// Default group: a name there logs a warning and uses it (ID 1).
  #[arg(long, default_value = "Default", value_name = "NAME|ID")]
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

/// `register`'s authentication flags: a personal token or the saved GitHub
/// App (#201).
#[derive(Debug, Args)]
pub(crate) struct RegisterAuthArgs {
  /// GitHub API token for the `generate-jitconfig` REST call.
  ///
  /// Needs admin rights on the target repo/org. Optional — resolution order:
  /// --app > this flag > `TOOLU_RUNNER_TOKEN` env > the GitHub App saved by
  /// `create-app` for the URL's host > the stored `login` token. Used only
  /// during registration, never at job runtime.
  #[arg(long, value_name = "TOKEN", conflicts_with = "app")]
  pub(crate) token: Option<String>,
  /// Authenticate with the GitHub App saved by `create-app`
  /// (`<home>/github-app.json`) instead of a personal token.
  ///
  /// Registration and every re-mint then use an App installation token, so
  /// the runner needs no stored login or `TOOLU_RUNNER_TOKEN`. Used
  /// automatically when that file exists for the URL's host and neither
  /// --token nor `TOOLU_RUNNER_TOKEN` is given. The App must be installed on
  /// the repository or organization.
  #[arg(long)]
  pub(crate) app: bool,
}

/// Arguments for the `run` subcommand.
#[derive(Debug, Args)]
pub(crate) struct RunArgs {
  /// Path to the runner config file. Default: inferred from the cwd git
  /// remote (runners/<owner>/<repo>/ under the runner home), else the
  /// sole existing registration.
  #[arg(long, value_name = "FILE", value_hint = ValueHint::FilePath)]
  pub(crate) config: Option<PathBuf>,
  /// Exit after the first job completes (single-job mode).
  ///
  /// The default is to stay online: after each job the runner re-mints a
  /// fresh JIT config (personal token, or the registration's GitHub App) and
  /// keeps listening.
  /// Pass this to run exactly one job and exit with its status.
  #[arg(long)]
  pub(crate) once: bool,
  /// Supervise every registration under the runner home in one process.
  ///
  /// What the machine service runs: one always-online loop per
  /// registration (each keeps its own job lock, backoff, re-mint and
  /// ephemeral rules); SIGHUP rescans the registrations without disturbing
  /// a running job.
  #[arg(long, conflicts_with_all = ["config", "once"])]
  pub(crate) all: bool,
}

/// Arguments for the `remove` subcommand.
#[derive(Debug, Args)]
pub(crate) struct RemoveArgs {
  /// Path to the runner config file. Default: inferred from the cwd git
  /// remote (runners/<owner>/<repo>/ under the runner home), else the
  /// sole existing registration.
  #[arg(long, value_name = "FILE", value_hint = ValueHint::FilePath)]
  pub(crate) config: Option<PathBuf>,
  /// GitHub token authorizing the unregister call.
  ///
  /// Precedence: this flag > `TOOLU_RUNNER_TOKEN` > the stored `login`
  /// token. With none of the three, `remove` FAILS without deleting
  /// anything — losing the persisted runner id would leave no way to
  /// unregister. Pass --skip-unregister to remove local state anyway.
  #[arg(long, value_name = "TOKEN")]
  pub(crate) token: Option<String>,
  /// Remove even while a run holds the job lock.
  ///
  /// Without it, a held lock writes a `.pending_remove` marker for the
  /// running process; the machine service stops an idle runner within
  /// seconds and remove then completes, otherwise remove exits 2. With it, local
  /// state goes but the lock file is KEPT (the running process still owns
  /// it) and the GitHub unregister is SKIPPED — that job is still
  /// reporting against the registration. It does not yet cancel the job.
  /// A leftover lock nobody holds is not "in flight" and removes normally.
  #[arg(long)]
  pub(crate) force: bool,
  /// Delete local state without calling GitHub.
  ///
  /// The escape hatch for when the unregister cannot succeed — revoked
  /// token, deleted repo, no network. Leaves the runner registered on
  /// GitHub; remove it there by hand.
  #[arg(long)]
  pub(crate) skip_unregister: bool,
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

/// Arguments for the `install-service` subcommand.
#[derive(Debug, Args)]
pub(crate) struct InstallServiceArgs {
  /// Install a unit for this one registration instead of the machine
  /// service.
  ///
  /// The registration dir names the unit: a per-repo config yields
  /// `io.toolu.runner.<owner>.<repo>` (launchd) /
  /// `toolu-runner-<owner>-<repo>.service` (systemd); an org config
  /// `io.toolu.runner.orgs.<org>` / `toolu-runner-orgs-<org>.service`.
  #[arg(long, value_name = "FILE", value_hint = ValueHint::FilePath)]
  pub(crate) config: Option<PathBuf>,
  /// Print the generated unit to stdout without writing or activating it.
  ///
  /// Conflicts with --remove. Use it to review the launchd/systemd unit
  /// before installing.
  #[arg(long, conflicts_with = "remove")]
  pub(crate) print: bool,
  /// Write the unit file but do not activate it.
  ///
  /// Prints the exact `launchctl` / `systemctl` command to load it later.
  #[arg(long)]
  pub(crate) no_activate: bool,
  /// Deactivate and delete a previously installed unit (idempotent).
  ///
  /// A missing unit reports nothing-to-do and exits 0.
  #[arg(long)]
  pub(crate) remove: bool,
  /// Linger choice for a non-root systemd user service.
  #[command(flatten)]
  pub(crate) linger: LingerArgs,
}

/// Unattended answers to the linger question.
#[derive(Debug, Args)]
pub(crate) struct LingerArgs {
  /// Enable systemd lingering when needed, so a user unit keeps running after
  /// logout and starts at boot. Skipped for root system units and launchd.
  #[arg(long)]
  pub(crate) enable_linger: bool,
  /// Answer yes to the linger question for unattended installation.
  #[arg(long)]
  pub(crate) yes: bool,
  /// A linger answer already decided by the caller (`setup` asks in its
  /// own service step): the installer then never prompts.
  #[arg(skip)]
  pub(crate) decided: Option<bool>,
}

/// Arguments for the `boot` subcommand: none — it is driven entirely by
/// `TOOLU_JITCONFIG` / `TOOLU_DEADLINE` env vars (see `--help`), by design
/// (a container ENTRYPOINT gets no argv).
#[derive(Debug, Args)]
#[command(after_help = BOOT_AFTER_HELP)]
pub(crate) struct BootArgs {}

/// Runner name from `--name`, defaulting to the machine hostname.
pub(crate) fn runner_name_or_hostname(name: Option<String>) -> String {
  name.unwrap_or_else(|| {
    hostname::get()
      .ok()
      .and_then(|h| h.into_string().ok())
      .unwrap_or_else(|| "toolu-runner".to_owned())
  })
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
  work.map_or_else(
    || "~/.toolu-runner/_work".to_owned(),
    |p| p.to_string_lossy().into_owned(),
  )
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
