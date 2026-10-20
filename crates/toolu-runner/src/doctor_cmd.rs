//! `doctor` subcommand: one read-only answer to "is this machine healthy as
//! a toolu runner, and how do I fix what is not?".
//!
//! Gathers the install, host, GitHub auth, registration and network
//! sections, renders them through a `SecretMasker` and returns an explicit
//! exit code that `main` applies directly (like `check` / `boot`): `0` no
//! `FAIL` row, `1` at least one, `2` a usage error before anything is
//! checked. Never takes a registration's lock, writes a file, opens a session
//! or registers; the only credentialed request is the auth section's
//! `GET <api>/user`, and `--offline` makes no request at all.

use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::{Args, ValueHint};
use config::registry;
use shared::SecretMasker;

use crate::check_cmd::{self, Target};
use crate::check_env::ProxyReport;
use crate::check_report;

use crate::doctor_auth;
use crate::doctor_host;
use crate::doctor_registrations::{Checked, check_registration};
use crate::doctor_report::{Level, Row, Section, exit_code, render};
use crate::doctor_service;

/// Exit code for a usage error before any check runs.
const EXIT_USAGE: i32 = 2;

/// Extended help footer for `doctor --help`.
const DOCTOR_AFTER_HELP: &str = "\
Sections: install (version, newer release), host (platform, uid, free disk,
systemd lingering), github auth (the bearer `run` re-mints with, verified by one
authenticated GET <api>/user per host), registrations (config, credentials,
JIT, whether `run` is serving it, supervisor unit, pending removal, last job)
and network (the same probes as `check`, per host). Each WARN/FAIL row is
followed by the command that fixes it.

Read-only: never takes a registration's lock, writes a file, opens a session or
registers. `check` is the narrower, credential-free network preflight.

Examples:
  toolu-runner doctor                    # every registration under TOOLU_RUNNER_HOME
  toolu-runner doctor --config <PATH>    # one registration
  toolu-runner doctor --offline          # local checks only: no network request

Exit codes:
  0  no FAIL row (warnings allowed)
  1  at least one FAIL row
  2  usage error before checking (e.g. a --config path that does not exist)

Environment:
  TOOLU_RUNNER_RELEASES_URL  release document the newer-release check reads
                             (default: Falconiere/toolu-ghrunner's latest
                             release on api.github.com)";

/// `doctor` arguments.
#[derive(Debug, Args)]
#[command(after_help = DOCTOR_AFTER_HELP)]
pub(crate) struct DoctorArgs {
  /// Check only this registration. Default: every registration under the
  /// runner home (`TOOLU_RUNNER_HOME`, else `~/.toolu-runner`).
  #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
  pub(crate) config: Option<PathBuf>,
  /// Make no network request; network rows print SKIP.
  #[arg(long)]
  pub(crate) offline: bool,
  /// Per-probe bound in seconds (1–300).
  #[arg(long, value_name = "SECS", default_value_t = 10,
        value_parser = clap::value_parser!(u64).range(1..=300))]
  pub(crate) timeout: u64,
}

/// The registrations to check, or why the registry could not be listed.
struct Selection {
  /// `config.toml` paths, in report order.
  configs: Vec<PathBuf>,
  /// The registry listing error, reported as a `FAIL` row.
  unreadable: Option<String>,
}

/// `--config` (which must exist) > every registration under `home`.
fn select(home: &Path, config: Option<&Path>) -> Result<Selection, String> {
  if let Some(path) = config {
    if !path.exists() {
      return Err(format!("config not found at {}", path.display()));
    }
    return Ok(Selection {
      configs: vec![path.to_path_buf()],
      unreadable: None,
    });
  }
  Ok(match registry::list_registrations(home) {
    Ok(entries) => Selection {
      configs: entries.into_iter().map(|entry| entry.config_path).collect(),
      unreadable: None,
    },
    Err(error) => Selection {
      configs: Vec::new(),
      unreadable: Some(error.to_string()),
    },
  })
}

/// The `registrations` section from each registration's checked block.
fn registrations_section(selection: &Selection, checked: &[Checked]) -> Section {
  if let Some(error) = &selection.unreadable {
    return Section::new(
      "registrations",
      vec![Row::new(Level::Fail, "registrations", error.clone())],
    );
  }
  if selection.configs.is_empty() {
    return Section::new(
      "registrations",
      vec![
        Row::new(Level::Info, "registrations", "none")
          .with_fix("toolu-runner register --url https://github.com/<owner>/<repo>"),
      ],
    );
  }
  let mut section = Section::new("registrations", Vec::new());
  for checked in checked {
    section.push_heading(checked.heading.clone());
    section.extend(checked.rows.iter().cloned());
  }
  section
}

/// One target per distinct registration host (its first registration
/// supplies the JIT endpoints); github.com when nothing loaded.
fn network_targets(selection: &Selection, checked: &[Checked]) -> Vec<Result<Target, String>> {
  let mut seen = Vec::new();
  let mut targets = Vec::new();
  for (path, block) in selection.configs.iter().zip(checked) {
    let Some(config) = &block.config else {
      continue;
    };
    let key = url::Url::parse(&config.runner_url).ok().map(|url| {
      (
        url.host_str().map(str::to_owned),
        url.port_or_known_default(),
      )
    });
    if !seen.contains(&key) {
      seen.push(key);
      targets.push(check_cmd::target_from_registration(path));
    }
  }
  if targets.is_empty() {
    targets.push(check_cmd::unregistered_target(
      "https://github.com",
      "no registration".to_owned(),
    ));
  }
  targets
}

/// `check`'s endpoint, tool, proxy and TLS lines for one target, indented
/// under a `network (<host>)` section; one `SKIP` row offline.
async fn network_section(
  target: Result<Target, String>,
  offline: bool,
  timeout: Duration,
  masker: &mut SecretMasker,
) -> Section {
  let target = match target {
    Ok(target) => target,
    Err(error) => {
      return Section::new(
        "network",
        vec![Row::new(
          Level::Skip,
          "endpoints",
          format!("registration unreadable (see registrations): {error}"),
        )],
      );
    },
  };
  let title = format!("network ({})", target.base.host_str().unwrap_or_default());
  if offline {
    return Section::new(&title, vec![Row::new(Level::Skip, "endpoints", "offline")]);
  }
  let proxy = ProxyReport::from_env();
  for secret in check_cmd::credential_secrets(&proxy, &target.network) {
    masker.add_secret(&secret);
  }
  match check_cmd::gather(&target, timeout, proxy).await {
    Ok(report) => Section::verbatim(
      &title,
      check_report::section_lines(&report)
        .into_iter()
        .map(|line| format!("  {line}"))
        .collect(),
      report.required_failures(),
      report.warnings(),
    ),
    Err(error) => Section::new(
      &title,
      vec![Row::new(
        Level::Fail,
        "network",
        format!("unusable [network] settings: {error}"),
      )],
    ),
  }
}

/// This machine's name for the report header.
fn host_name() -> String {
  hostname::get().map_or_else(
    |_| "this machine".to_owned(),
    |name| name.to_string_lossy().into_owned(),
  )
}

/// Run `doctor`; returns the process exit code.
pub(crate) async fn cmd_doctor(args: DoctorArgs) -> i32 {
  let home = registry::runner_home();
  let selection = match select(&home, args.config.as_deref()) {
    Ok(selection) => selection,
    Err(error) => {
      eprintln!("toolu-runner: {error}");
      return EXIT_USAGE;
    },
  };
  let timeout = Duration::from_secs(args.timeout);
  let mut masker = SecretMasker::new();
  let update = doctor_host::update_check(args.offline, timeout).await;
  let mut checked: Vec<Checked> = Vec::new();
  for config in &selection.configs {
    checked.push(check_registration(config, timeout).await);
  }
  let mut disk_paths = vec![home];
  disk_paths.extend(checked.iter().filter_map(|block| block.work_dir.clone()));
  let mut host = vec![doctor_host::platform_row(), doctor_host::user_row()];
  host.extend(doctor_host::disk_rows(&disk_paths));
  let any_unit = checked.iter().any(|block| block.unit_installed);
  host.extend(doctor_service::linger_row(any_unit, timeout).await);
  let mut sections = vec![
    Section::new("install", vec![doctor_host::version_row(), update]),
    Section::new("host", host),
  ];
  let configs: Vec<_> = checked
    .iter()
    .filter_map(|block| block.config.as_ref())
    .collect();
  for target in doctor_auth::auth_hosts(&configs) {
    sections.push(doctor_auth::auth_section(&target, args.offline, timeout, &mut masker).await);
  }
  sections.push(registrations_section(&selection, &checked));
  for target in network_targets(&selection, &checked) {
    sections.push(network_section(target, args.offline, timeout, &mut masker).await);
  }
  print!("{}", render(&host_name(), &sections, &masker));
  exit_code(&sections)
}
