//! `setup` flags: every interactive prompt has one (epic #199's flag
//! table). Grammar and cross-flag rules are validated in
//! [`super::plan`], not by clap, so a missing answer can exit 2 naming
//! the exact flag setup needed.

use std::path::PathBuf;

use clap::Args;

/// Arguments for the `setup` subcommand.
#[derive(Debug, Args)]
pub(crate) struct SetupArgs {
  /// Where jobs come from: `github`, or `toolu` (pair with a toolu.sh
  /// workspace).
  #[arg(long, value_name = "SOURCE")]
  pub(crate) source: Option<String>,
  /// Pair with toolu.sh using a single-use enrollment token minted in the
  /// console (implies `--source toolu`; asks nothing). Once the source is
  /// toolu.sh (this flag, `--source toolu` or the prompt),
  /// `TOOLU_RUNNER_ENROLL_TOKEN` is read when this is absent (keeps the token
  /// out of `ps`); the variable alone never picks the source.
  #[arg(long, value_name = "TOKEN")]
  pub(crate) enroll_token: Option<String>,
  /// Register an organization-level runner for this GitHub organization.
  #[arg(long, value_name = "ORG", conflicts_with = "repo")]
  pub(crate) org: Option<String>,
  /// Register a repository runner for `owner/name` (repeatable, one
  /// owner per run).
  #[arg(long, value_name = "OWNER/REPO")]
  pub(crate) repo: Vec<String>,
  /// GitHub web base URL (`https://github.com`, a GHES base, or a
  /// loopback `http://127.0.0.1:<port>` peer in tests).
  #[arg(long, value_name = "URL")]
  pub(crate) github_url: Option<String>,
  /// How to authenticate: `app` (create or reuse a GitHub App),
  /// `import` (an existing App), or `token` (a personal access token).
  #[arg(long, value_name = "KIND")]
  pub(crate) auth: Option<String>,
  /// Reuse an existing App from a `github-app.json` file (`--auth import`).
  #[arg(long, value_name = "FILE")]
  pub(crate) import: Option<PathBuf>,
  /// Reuse an existing App by id, with `--app-key` (`--auth import`).
  #[arg(long, value_name = "ID")]
  pub(crate) app_id: Option<i64>,
  /// The existing App's private-key PEM file, with `--app-id`.
  #[arg(long, value_name = "PEM_FILE")]
  pub(crate) app_key: Option<PathBuf>,
  /// Personal access token (warns: the value may land in shell history;
  /// prefer `TOOLU_RUNNER_TOKEN` or `--token-stdin`).
  #[arg(long, value_name = "TOKEN", conflicts_with = "token_stdin")]
  pub(crate) token: Option<String>,
  /// Read the personal access token from stdin (not usable under
  /// `curl … | bash`, where stdin is the script).
  #[arg(long)]
  pub(crate) token_stdin: bool,
  /// Runner group for an organization registration (default: Default).
  #[arg(long, value_name = "NAME|ID")]
  pub(crate) runner_group: Option<String>,
  /// Runner name (default: the machine hostname).
  #[arg(long, value_name = "NAME")]
  pub(crate) name: Option<String>,
  /// Extra labels, comma-separated, added to the defaults.
  #[arg(long, value_name = "LABELS", value_delimiter = ',')]
  pub(crate) labels: Vec<String>,
  /// Register only the `--labels` values, without the defaults.
  #[arg(long)]
  pub(crate) no_default_labels: bool,
  /// Working directory for job workspaces (default: ~/.toolu-runner/_work).
  #[arg(long, value_name = "DIR")]
  pub(crate) work_dir: Option<PathBuf>,
  /// Registration-mode flags (ephemeral / replace / no service).
  #[command(flatten)]
  pub(crate) mode: SetupMode,
  /// Flow flags (linger / unattended / dry run).
  #[command(flatten)]
  pub(crate) flow: SetupFlow,
}

/// Registration-mode flags.
#[derive(Debug, Args)]
pub(crate) struct SetupMode {
  /// One job, then the runner unregisters and deletes itself.
  #[arg(long)]
  pub(crate) ephemeral: bool,
  /// Replace a runner or registration with the same name.
  #[arg(long)]
  pub(crate) replace: bool,
  /// Do not install the background machine service.
  #[arg(long)]
  pub(crate) no_service: bool,
}

/// Flow flags.
#[derive(Debug, Args)]
pub(crate) struct SetupFlow {
  /// Enable systemd lingering with the service (non-root Linux).
  #[arg(long)]
  pub(crate) enable_linger: bool,
  /// Do not ask: every answer must come from a flag, and the Review is
  /// skipped. A missing answer exits 2 naming its flag.
  #[arg(long)]
  pub(crate) yes: bool,
  /// Print the Review and exit without writing anything.
  #[arg(long)]
  pub(crate) dry_run: bool,
}

impl SetupArgs {
  /// `--yes`: unattended, every answer from a flag.
  pub(crate) fn yes(&self) -> bool {
    self.flow.yes
  }

  /// `--dry-run`: print the Review and exit.
  pub(crate) fn dry_run(&self) -> bool {
    self.flow.dry_run
  }
}
