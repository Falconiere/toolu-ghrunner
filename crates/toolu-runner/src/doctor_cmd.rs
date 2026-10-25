//! `doctor` subcommand: one read-only answer to "is this machine healthy as
//! a toolu runner, and how do I fix what is not?".
//!
//! Gathers the install, host, GitHub auth, registration and network
//! sections (plus a toolu.sh section per paired machine, #207), renders them
//! through a `SecretMasker` and returns an explicit
//! exit code that `main` applies directly (like `check` / `boot`): `0` no
//! `FAIL` row, `1` at least one, `2` a usage error before anything is
//! checked. Never takes a registration's lock, writes a file, opens a session
//! or registers; the only credentialed requests are the auth section's
//! `GET <api>/user` and a toolu.sh machine's `GET /runner/host`, and
//! `--offline` makes no request at all.

use std::path::{Component, Path, PathBuf};
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
JIT, whether `run` is serving it, supervisor unit, pending removal, last job),
toolu.sh (a paired machine as toolu.sh sees it: one GET /runner/host with its
host token) and network (the same probes as `check`, per GitHub host). Each
WARN/FAIL row is
followed by the command that fixes it, where one exists.

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

/// `path` with `.` and `..` folded lexically. No filesystem access, so a
/// symlinked registration dir keeps its `runners/<owner>/<repo>` name.
fn fold_dots(path: &Path) -> PathBuf {
  let mut folded = PathBuf::new();
  for part in path.components() {
    match part {
      Component::CurDir => {},
      Component::ParentDir => {
        folded.pop();
      },
      other @ (Component::Prefix(_) | Component::RootDir | Component::Normal(_)) => {
        folded.push(other);
      },
    }
  }
  folded
}

/// `--config` (which must exist; made absolute and folded so its
/// `runners/<owner>/<repo>` identity is visible) > every registration under
/// `home`.
fn select(home: &Path, config: Option<&Path>) -> Result<Selection, String> {
  if let Some(path) = config {
    if !path.exists() {
      return Err(format!("config not found at {}", path.display()));
    }
    let path = std::path::absolute(path)
      .map_err(|error| format!("cannot resolve {}: {error}", path.display()))?;
    return Ok(Selection {
      configs: vec![fold_dots(&path)],
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

/// The registration's target, and whether it fell back: when its JIT config
/// does not decode, the host's generic endpoints through its `[network]`, so
/// the host is still probed (the `jit` row already reports the broken config).
fn registration_target(
  path: &Path,
  config: &config::config::RunnerRegistrationConfig,
) -> (Result<Target, String>, bool) {
  if let Ok(target) = check_cmd::target_from_registration(path) {
    return (Ok(target), false);
  }
  let origin = format!(
    "registration: {} (JIT unreadable; generic endpoints)",
    path.display()
  );
  let target = check_cmd::unregistered_target(&config.runner_url, origin).map(|mut target| {
    target.network = config.network_config();
    target
  });
  (target, true)
}

/// A registration host: its name and port.
type HostKey = Option<(Option<String>, Option<u16>)>;

/// One target per distinct registration host, from the first registration on
/// it whose JIT config decodes (else the first one's generic endpoints);
/// github.com when nothing loaded.
fn network_targets(selection: &Selection, checked: &[Checked]) -> Vec<Result<Target, String>> {
  let mut hosts: Vec<(HostKey, Result<Target, String>, bool)> = Vec::new();
  for (path, block) in selection.configs.iter().zip(checked) {
    let Some(config) = &block.config else {
      continue;
    };
    // A toolu.sh machine talks to toolu.sh, not GitHub: its own section.
    if !config.provider.is_github() {
      continue;
    }
    let key: HostKey = url::Url::parse(&config.runner_url).ok().map(|url| {
      (
        url.host_str().map(str::to_owned),
        url.port_or_known_default(),
      )
    });
    if let Some((_, target, generic)) = hosts.iter_mut().find(|(known, ..)| *known == key) {
      if *generic && let Ok(own) = check_cmd::target_from_registration(path) {
        *target = Ok(own);
        *generic = false;
      }
    } else {
      let (target, generic) = registration_target(path, config);
      hosts.push((key, target, generic));
    }
  }
  let mut targets: Vec<Result<Target, String>> =
    hosts.into_iter().map(|(_, target, _)| target).collect();
  if targets.is_empty()
    && !checked.iter().any(|block| {
      block
        .config
        .as_ref()
        .is_some_and(|config| !config.provider.is_github())
    })
  {
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
      // A line can carry a second `hint:` line; indent every physical line.
      check_report::section_lines(&report)
        .iter()
        .flat_map(|line| line.lines().map(|part| format!("  {part}")))
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
  for secret in network::url_credentials(&crate::config::releases_url()) {
    masker.add_secret(&secret);
  }
  let mut checked: Vec<Checked> = Vec::new();
  for config in &selection.configs {
    checked.push(check_registration(config, timeout).await);
  }
  // The release check goes through the first loadable registration's
  // `[network]`, like every other runner client (env mode without one).
  let release_network = checked
    .iter()
    .find_map(|block| block.config.as_ref())
    .map(config::config::RunnerRegistrationConfig::network_config)
    .unwrap_or_default();
  let update = doctor_host::update_check(args.offline, timeout, &release_network).await;
  let mut disk_paths = vec![home];
  disk_paths.extend(checked.iter().filter_map(|block| block.work_dir.clone()));
  // `id -u` is a blocking subprocess: keep it off the async workers.
  let user = tokio::task::spawn_blocking(doctor_host::user_row)
    .await
    .unwrap_or_else(|error| {
      Row::new(
        Level::Warn,
        "user",
        format!("could not determine the uid: {error}"),
      )
    });
  let mut host = vec![doctor_host::platform_row(), user];
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
  for (path, block) in selection.configs.iter().zip(&checked) {
    if let Some(config) = &block.config
      && let Some(section) =
        crate::doctor_toolu::section(path, config, args.offline, timeout, &mut masker).await
    {
      sections.push(section);
    }
  }
  for target in network_targets(&selection, &checked) {
    sections.push(network_section(target, args.offline, timeout, &mut masker).await);
  }
  print!("{}", render(&host_name(), &sections, &masker));
  exit_code(&sections)
}
