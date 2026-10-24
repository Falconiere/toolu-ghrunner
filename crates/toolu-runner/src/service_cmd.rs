//! `install-service` subcommand: generate + activate a supervisor unit.
//!
//! Without `--config` it installs the machine unit that runs `run --all`
//! ([`crate::machine_service`], #208). With `--config` it writes a launchd
//! `LaunchAgent` (macOS) or systemd user/system unit (Linux) that wraps
//! `run --config <path>` so one registration survives crashes and reboots.
//! The unit text comes from `config::service_unit`; this module owns config
//! resolution, service identity, file destinations, and the platform
//! activation shell-outs (`launchctl` / `systemctl`). No network, no
//! tracing init — like `status`, it only touches local files and subprocesses.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use config::config::{load_config as load_reg_config, resolve_data_dir};
use config::service_unit::{
  ServiceAction, ServiceSpec, Supervises, launchd_env_path, launchd_plist, parse_systemd_target,
  service_action_summary, systemd_system_unit, systemd_unit, systemd_unit_with_home,
};
use shared::RunnerError;

use crate::cli::InstallServiceArgs;

/// Linger preflight for non-root systemd installs.
#[path = "service_linger.rs"]
pub(crate) mod service_linger;

/// The OS supervisor a unit targets. Selected at runtime from the build's
/// target OS; anything but macOS/Linux is rejected before any work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Supervisor {
  /// macOS launchd user `LaunchAgent`.
  Launchd,
  /// Linux systemd user or system unit.
  Systemd,
}

/// Whether a unit belongs to a user manager or the system manager.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UnitScope {
  /// launchd or `systemctl --user`.
  User,
  /// Root-owned systemd unit under `/etc/systemd/system`.
  System,
}

/// A registration's supervisor identity: the launchd `Label` (also the
/// systemd `Description` suffix) and the systemd unit file basename.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ServiceId {
  /// launchd `Label` + plist basename stem; systemd `Description` suffix.
  pub(crate) label: String,
  /// systemd unit file basename (`toolu-runner-<owner>-<repo>.service`).
  pub(crate) unit: String,
}

/// `install-service`: without `--config`, the machine unit
/// ([`crate::machine_service::cmd_install_machine`]); with it, derive the
/// unit for that registration, then print / write / activate / remove it per
/// the flags.
pub(crate) fn cmd_install_service(
  args: &InstallServiceArgs,
) -> Result<(), Box<dyn std::error::Error>> {
  let Some(config_path) = args.config.clone() else {
    return crate::machine_service::cmd_install_machine(args);
  };
  let supervisor = current_supervisor()?;
  let scope = current_unit_scope(supervisor)?;
  let id = service_id(&config_path);

  // `--print` writes nothing, so it renders to stdout and
  // returns before the core touches the filesystem (clap forbids `--print`
  // with `--remove`).
  if args.print {
    print!("{}", render_unit(supervisor, scope, &config_path, &id)?);
    return Ok(());
  }

  // The core does the filesystem + activation work and returns a
  // self-describing outcome; the caller prints from `outcome.action` alone
  // (never re-checking the flags it passed in). The status line is owned by
  // the pure `service_action_summary`, so the CLI output lives in one place.
  let outcome = install_service_core(supervisor, scope, &config_path, args)?;
  let summary = service_action_summary(&outcome.action, &outcome.label, &outcome.path);
  if !summary.is_empty() {
    println!("{summary}");
  }
  // A written-but-not-activated unit needs a follow-on activation command.
  // It depends on the supervisor + unit id, which the pure summary can't
  // compute, so it is printed here — keyed off the outcome, not the flags.
  if matches!(
    outcome.action,
    ServiceAction::Installed { activated: false }
  ) {
    println!(
      "activate it with:\n  {}",
      activation_hint(supervisor, scope, &outcome.path, &id)
    );
  }
  Ok(())
}

/// The outcome of [`install_service_core`] — what was written / removed, so
/// [`cmd_install_service`] can print the result without doing the work itself.
pub(crate) struct InstallOutcome {
  /// The unit file path acted on (empty in the `print` short-circuit).
  pub path: PathBuf,
  /// The service label (launchd `Label` / systemd `Description` suffix).
  pub label: String,
  /// What the core did, so the caller prints the right message without
  /// re-inspecting the flags. Encodes the `--no-activate` vs activated split
  /// (`ServiceAction::Installed { activated }`), the two `--remove` cases,
  /// and the `print` short-circuit.
  pub action: ServiceAction,
}

/// The write / activate / remove work behind `install-service`, with all
/// status printing lifted to [`cmd_install_service`]. Resolves the
/// supervisor and service id from `config_path`, acts per the flags, and
/// returns the [`InstallOutcome`] — it never prints. `print` short-circuits
/// before any filesystem work (nothing is written — the
/// caller renders the unit to stdout); `remove` deactivates + deletes;
/// otherwise the unit is written and, unless `no_activate`, activated. Keeps
/// the launchctl / systemctl activation shell-outs.
pub(crate) fn install_service_core(
  supervisor: Supervisor,
  scope: UnitScope,
  config_path: &Path,
  args: &InstallServiceArgs,
) -> Result<InstallOutcome, RunnerError> {
  let id = service_id(config_path);

  // `--print` writes nothing and needs no HOME: return before the destination
  // is computed. The caller renders the unit text to stdout.
  if args.print {
    return Ok(InstallOutcome {
      path: PathBuf::new(),
      label: id.label,
      action: ServiceAction::Printed,
    });
  }

  let home = home_dir()?;
  let dest = dest_path_scoped(supervisor, scope, &id, &home);
  if args.remove {
    return remove_registration_service(supervisor, scope, config_path, &id, &home, &dest);
  }

  let unit_text = render_unit(supervisor, scope, config_path, &id)?;
  if supervisor == Supervisor::Systemd && scope == UnitScope::System {
    check_existing_system_unit(&dest, config_path)?;
  }
  let linger = service_linger::before_install(supervisor, scope, args)?;
  write_unit(&dest, &unit_text)?;
  if args.no_activate {
    service_linger::warn_if_short_lived(linger);
    return Ok(InstallOutcome {
      path: dest,
      label: id.label,
      action: ServiceAction::Installed { activated: false },
    });
  }
  activate(supervisor, scope, &dest, &id)?;
  service_linger::warn_if_short_lived(linger);
  Ok(InstallOutcome {
    path: dest,
    label: id.label,
    action: ServiceAction::Installed { activated: true },
  })
}

/// A root per-registration install may reuse its own unit, but cannot write
/// through a symlink or overwrite a live registration with the same basename.
fn check_existing_system_unit(dest: &Path, config_path: &Path) -> Result<(), RunnerError> {
  let metadata = match std::fs::symlink_metadata(dest) {
    Ok(metadata) => metadata,
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
    Err(error) => return Err(error.into()),
  };
  if metadata.file_type().is_symlink() {
    return Err(RunnerError::Config(format!(
      "{} is a symlink; remove it before installing so its target is not overwritten",
      dest.display()
    )));
  }
  let text = std::fs::read_to_string(dest)?;
  if let Some(Supervises::Config(existing)) = parse_systemd_target(&text)
    && (!existing.exists() || same_path(&existing, config_path))
  {
    return Ok(());
  }
  Err(RunnerError::Config(format!(
    "{} already runs another live service; nothing was changed",
    dest.display()
  )))
}

fn same_path(a: &Path, b: &Path) -> bool {
  match (a.canonicalize(), b.canonicalize()) {
    (Ok(a), Ok(b)) => a == b,
    _ => a == b,
  }
}

fn remove_registration_service(
  supervisor: Supervisor,
  scope: UnitScope,
  config_path: &Path,
  id: &ServiceId,
  home: &Path,
  dest: &Path,
) -> Result<InstallOutcome, RunnerError> {
  let outcome = remove_service(supervisor, scope, dest, id)?;
  // Before #208 a legacy-root registration's unit was `toolu-runner.service`,
  // now the machine unit's name: remove that file too when it runs this
  // registration (never when it is the machine unit).
  if let Some(old) = pre_208_legacy_unit(supervisor, config_path, id, home) {
    let old_id = ServiceId {
      label: id.label.clone(),
      unit: config::service_unit::MACHINE_UNIT.to_owned(),
    };
    let removed = remove_service(supervisor, UnitScope::User, &old, &old_id)?;
    if removed.action == ServiceAction::Removed {
      return Ok(removed);
    }
  }
  Ok(outcome)
}

/// The supervisor for the build's target OS: launchd on macOS, systemd on
/// Linux. Any other target errors — Windows service support is out of scope.
pub(crate) fn current_supervisor() -> Result<Supervisor, RunnerError> {
  if cfg!(target_os = "macos") {
    Ok(Supervisor::Launchd)
  } else if cfg!(target_os = "linux") {
    Ok(Supervisor::Systemd)
  } else {
    Err(RunnerError::Config(
      "install-service supports only launchd (macOS) and systemd (Linux)".to_owned(),
    ))
  }
}

/// Root uses the system manager; other Linux users and launchd use their
/// user manager. `id -u` reports the effective UID, including under `sudo`.
pub(crate) fn current_unit_scope(supervisor: Supervisor) -> Result<UnitScope, RunnerError> {
  match supervisor {
    Supervisor::Launchd => Ok(UnitScope::User),
    Supervisor::Systemd => Ok(if current_uid()? == "0" {
      UnitScope::System
    } else {
      UnitScope::User
    }),
  }
}

/// Derive the per-registration service identity from a config path. A
/// per-repo config (`.../runners/<owner>/<repo>/config.toml`) yields
/// `io.toolu.runner.<owner>.<repo>` / `toolu-runner-<owner>-<repo>.service`;
/// an org config (`.../orgs/<org>/config.toml`) yields
/// `io.toolu.runner.orgs.<org>` / `toolu-runner-orgs-<org>.service`; any
/// other layout (the legacy `<home>/config.toml`) falls back to
/// `io.toolu.runner` / `toolu-runner-legacy.service` — `toolu-runner.service`
/// is the machine unit's name since #208.
pub(crate) fn service_id(config_path: &Path) -> ServiceId {
  if let Some((owner, repo)) = owner_repo(config_path) {
    return ServiceId {
      label: format!("io.toolu.runner.{owner}.{repo}"),
      unit: format!("toolu-runner-{owner}-{repo}.service"),
    };
  }
  if let Some(org) = org_of(config_path) {
    return ServiceId {
      label: format!("io.toolu.runner.orgs.{org}"),
      unit: format!("toolu-runner-orgs-{org}.service"),
    };
  }
  ServiceId {
    label: "io.toolu.runner".to_owned(),
    unit: "toolu-runner-legacy.service".to_owned(),
  }
}

/// The org of a `.../orgs/<org>/config.toml` path, or `None`.
pub(crate) fn org_of(config_path: &Path) -> Option<String> {
  let org_dir = config_path.parent()?;
  if org_dir.parent()?.file_name()?.to_str()? != "orgs" {
    return None;
  }
  Some(org_dir.file_name()?.to_str()?.to_owned())
}

/// A pre-#208 systemd unit for a legacy-root registration: the user's
/// `toolu-runner.service` when it runs `run --config <config_path>`.
fn pre_208_legacy_unit(
  supervisor: Supervisor,
  config_path: &Path,
  id: &ServiceId,
  home: &Path,
) -> Option<PathBuf> {
  if supervisor != Supervisor::Systemd || id.unit != "toolu-runner-legacy.service" {
    return None;
  }
  let old = home
    .join(".config/systemd/user")
    .join(config::service_unit::MACHINE_UNIT);
  let text = std::fs::read_to_string(&old).ok()?;
  let config::service_unit::Supervises::Config(path) =
    config::service_unit::parse_systemd_target(&text)?
  else {
    return None;
  };
  let same = path == config_path || path.canonicalize().ok() == config_path.canonicalize().ok();
  same.then_some(old)
}

/// Extract `(owner, repo)` from a `.../runners/<owner>/<repo>/config.toml`
/// path, or `None` when the config is not under a `runners/` tree (legacy
/// root registration).
pub(crate) fn owner_repo(config_path: &Path) -> Option<(String, String)> {
  let repo_dir = config_path.parent()?;
  let owner_dir = repo_dir.parent()?;
  if owner_dir.parent()?.file_name()?.to_str()? != "runners" {
    return None;
  }
  let owner = owner_dir.file_name()?.to_str()?.to_owned();
  let repo = repo_dir.file_name()?.to_str()?.to_owned();
  Some((owner, repo))
}

/// OS-conventional destination for the unit file, honoring `$HOME`.
/// macOS: `~/Library/LaunchAgents/<label>.plist`; Linux:
/// `~/.config/systemd/user/<unit>` (parent dirs created by [`write_unit`]).
pub(crate) fn dest_path(supervisor: Supervisor, id: &ServiceId, home: &Path) -> PathBuf {
  match supervisor {
    Supervisor::Launchd => home
      .join("Library/LaunchAgents")
      .join(format!("{}.plist", id.label)),
    Supervisor::Systemd => home.join(".config/systemd/user").join(&id.unit),
  }
}

/// A root systemd unit lives under `/etc/systemd/system`; every other unit
/// uses the existing user destination.
pub(crate) fn dest_path_scoped(
  supervisor: Supervisor,
  scope: UnitScope,
  id: &ServiceId,
  home: &Path,
) -> PathBuf {
  if supervisor == Supervisor::Systemd && scope == UnitScope::System {
    Path::new(crate::machine_service::SYSTEM_UNIT_DIR).join(&id.unit)
  } else {
    dest_path(supervisor, id, home)
  }
}

/// The user's home from `$HOME`; unit files land under it.
pub(crate) fn home_dir() -> Result<PathBuf, RunnerError> {
  std::env::var_os("HOME").map(PathBuf::from).ok_or_else(|| {
    RunnerError::Config("HOME is not set — cannot locate the service home".to_owned())
  })
}

/// Load `config_path`, derive its `_diag` dir, and render the platform unit
/// text. `exe` is the absolute running binary; `config_path` is canonicalized
/// so the supervisor's `ExecStart` / `ProgramArguments` are host-absolute.
fn render_unit(
  supervisor: Supervisor,
  scope: UnitScope,
  config_path: &Path,
  id: &ServiceId,
) -> Result<String, RunnerError> {
  let data_dir = service_data_dir(config_path)?;
  let diag_dir = data_dir.join("_diag");
  // Steps inherit the runner process's PATH under either supervisor.
  let env_path = launchd_env_path(crate::config::path().as_deref());
  if supervisor == Supervisor::Systemd {
    validate_unit_environment("PATH", &env_path)?;
  }
  let exe = std::env::current_exe()
    .map_err(|e| RunnerError::Config(format!("cannot resolve the running binary's path: {e}")))?;
  let abs_config = config_path.canonicalize().map_err(|e| {
    RunnerError::Config(format!(
      "cannot resolve config path {}: {e}",
      config_path.display()
    ))
  })?;
  let spec = ServiceSpec {
    label: &id.label,
    exe: &exe,
    config_path: &abs_config,
    diag_dir: &diag_dir,
    env_path: &env_path,
    work_dir: &data_dir,
  };
  Ok(match supervisor {
    Supervisor::Launchd => launchd_plist(&spec),
    Supervisor::Systemd => render_systemd_registration(&spec, scope)?,
  })
}

fn render_systemd_registration(
  spec: &ServiceSpec<'_>,
  scope: UnitScope,
) -> Result<String, RunnerError> {
  let runner_home = std::path::absolute(config::registry::runner_home())?;
  if scope == UnitScope::System {
    let user_home = home_dir()?;
    validate_unit_environment("HOME", &user_home.to_string_lossy())?;
    validate_unit_environment("TOOLU_RUNNER_HOME", &runner_home.to_string_lossy())?;
    return Ok(systemd_system_unit(spec, &user_home, &runner_home));
  }
  if config::registry::runner_home_is_custom() {
    validate_unit_environment("TOOLU_RUNNER_HOME", &runner_home.to_string_lossy())?;
    return Ok(systemd_unit_with_home(spec, &runner_home));
  }
  Ok(systemd_unit(spec))
}

fn service_data_dir(config_path: &Path) -> Result<PathBuf, RunnerError> {
  let cfg = load_reg_config(config_path)?;
  if cfg.ephemeral {
    return Err(RunnerError::Config(format!(
      "{} is an ephemeral registration (one job, then it unregisters and deletes itself); a \
       service unit restarts `run` forever and would relaunch into a removed registration — \
       start `toolu-runner run` from a one-shot supervisor instead, or register without \
       --ephemeral",
      config_path.display()
    )));
  }
  resolve_data_dir(&cfg.runtime.data_dir)
}

/// Reject line separators before placing an environment value into a
/// systemd unit, where they could add another directive.
pub(crate) fn validate_unit_environment(key: &str, value: &str) -> Result<(), RunnerError> {
  if value.contains('\n') || value.contains('\r') {
    return Err(RunnerError::Config(format!(
      "{key} contains a line break and cannot be written to a systemd unit"
    )));
  }
  Ok(())
}

/// Write the unit text to `dest`, creating parent dirs (0644 content).
pub(crate) fn write_unit(dest: &Path, unit: &str) -> Result<(), std::io::Error> {
  if let Some(parent) = dest.parent() {
    std::fs::create_dir_all(parent)?;
  }
  std::fs::write(dest, unit)
}

/// `--remove`: deactivate the unit (best-effort) and delete the file.
/// Idempotent — a missing file returns `ServiceAction::NothingToRemove` (the
/// caller reports nothing-to-do). Deletion failure IS an error; deactivation
/// failure (unit never loaded) is not. Never prints — printing is the caller's
/// job.
pub(crate) fn remove_service(
  supervisor: Supervisor,
  scope: UnitScope,
  dest: &Path,
  id: &ServiceId,
) -> Result<InstallOutcome, RunnerError> {
  if !dest.exists() {
    return Ok(InstallOutcome {
      path: dest.to_owned(),
      label: id.label.clone(),
      action: ServiceAction::NothingToRemove,
    });
  }
  deactivate(supervisor, scope, dest, id);
  // Tolerate a concurrent delete between the exists() check and here —
  // idempotency must hold under the race, not just sequentially.
  match std::fs::remove_file(dest) {
    Ok(()) => Ok(InstallOutcome {
      path: dest.to_owned(),
      label: id.label.clone(),
      action: ServiceAction::Removed,
    }),
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(InstallOutcome {
      path: dest.to_owned(),
      label: id.label.clone(),
      action: ServiceAction::NothingToRemove,
    }),
    Err(e) => Err(e.into()),
  }
}

/// Activate the just-written unit. A non-zero manager exit is an error
/// naming the command and its stderr (never swallowed).
pub(crate) fn activate(
  supervisor: Supervisor,
  scope: UnitScope,
  dest: &Path,
  id: &ServiceId,
) -> Result<(), RunnerError> {
  match supervisor {
    Supervisor::Launchd => activate_launchd(dest),
    Supervisor::Systemd => activate_systemd(&id.unit, scope),
  }
}

/// Load a plist into the per-user launchd domain. `bootstrap gui/<uid>` is
/// the modern (macOS 10.13+) loader; a non-zero exit (older host) falls back
/// to the legacy `launchctl load -w`, whose failure surfaces as the error.
fn activate_launchd(plist: &Path) -> Result<(), RunnerError> {
  let domain = format!("gui/{}", current_uid()?);
  let plist = plist.to_string_lossy().into_owned();
  if succeeds("launchctl", &["bootstrap", &domain, &plist]) {
    return Ok(());
  }
  run_checked("launchctl", &["load", "-w", &plist])
}

/// Reload the user manager and enable + start the unit.
fn activate_systemd(unit: &str, scope: UnitScope) -> Result<(), RunnerError> {
  match scope {
    UnitScope::User => {
      run_checked("systemctl", &["--user", "daemon-reload"])?;
      run_checked("systemctl", &["--user", "enable", "--now", unit])
    },
    UnitScope::System => {
      run_checked("systemctl", &["daemon-reload"])?;
      run_checked("systemctl", &["enable", "--now", unit])
    },
  }
}

/// Best-effort deactivation for `--remove`: a failure here (unit never
/// loaded, unknown to the manager) is reported, not fatal — deleting the
/// file is what removal guarantees.
pub(crate) fn deactivate(supervisor: Supervisor, scope: UnitScope, dest: &Path, id: &ServiceId) {
  if let Err(e) = deactivate_checked(supervisor, scope, dest, id) {
    eprintln!("warning: deactivation failed (unit may not have been loaded): {e}");
  }
}

/// Deactivate the unit, returning the supervisor's failure — for callers
/// that must not delete a unit file whose process may still run.
pub(crate) fn deactivate_checked(
  supervisor: Supervisor,
  scope: UnitScope,
  dest: &Path,
  id: &ServiceId,
) -> Result<(), RunnerError> {
  match supervisor {
    Supervisor::Launchd => deactivate_launchd(dest, &id.label),
    Supervisor::Systemd => deactivate_systemd(&id.unit, scope),
  }
}

/// Unload a plist from the per-user launchd domain: modern
/// `bootout gui/<uid>/<label>`, falling back to legacy `launchctl unload`.
fn deactivate_launchd(plist: &Path, label: &str) -> Result<(), RunnerError> {
  let target = format!("gui/{}/{label}", current_uid()?);
  if succeeds("launchctl", &["bootout", &target]) {
    return Ok(());
  }
  let plist_s = plist.to_string_lossy();
  run_checked("launchctl", &["unload", &plist_s])
}

/// Disable + stop the unit, then reload the user manager.
fn deactivate_systemd(unit: &str, scope: UnitScope) -> Result<(), RunnerError> {
  match scope {
    UnitScope::User => {
      run_checked("systemctl", &["--user", "disable", "--now", unit])?;
      run_checked("systemctl", &["--user", "daemon-reload"])
    },
    UnitScope::System => {
      run_checked("systemctl", &["disable", "--now", unit])?;
      run_checked("systemctl", &["daemon-reload"])
    },
  }
}

/// The exact command `--no-activate` tells the user to run — it mirrors what
/// the default mode would execute to load the just-written unit.
pub(crate) fn activation_hint(
  supervisor: Supervisor,
  scope: UnitScope,
  dest: &Path,
  id: &ServiceId,
) -> String {
  match (supervisor, scope) {
    (Supervisor::Launchd, _) => format!("launchctl bootstrap gui/$(id -u) {}", dest.display()),
    (Supervisor::Systemd, UnitScope::User) => format!(
      "systemctl --user daemon-reload && systemctl --user enable --now {}",
      id.unit
    ),
    (Supervisor::Systemd, UnitScope::System) => format!(
      "systemctl daemon-reload && systemctl enable --now {}",
      id.unit
    ),
  }
}

/// The current user id via `id -u` (avoids an `unsafe` libc `getuid`),
/// validated as all-digits — a broken `id` wrapper must fail loudly here,
/// not as a cryptic `launchctl` domain error downstream.
pub(crate) fn current_uid() -> Result<String, RunnerError> {
  let uid = run_stdout("id", &["-u"])?;
  if uid.is_empty() || !uid.bytes().all(|b| b.is_ascii_digit()) {
    return Err(RunnerError::Config(format!(
      "`id -u` returned a non-numeric uid: {uid:?}"
    )));
  }
  Ok(uid)
}

/// Run `program args`, capturing output. An IO failure (e.g. the binary is
/// missing) is an error naming the command line. Runner secrets are dropped
/// from the child env — `launchctl`/`systemctl` don't need them (full
/// `env_clear` would break `systemctl --user`, which needs XDG/DBus vars).
fn run_capture(program: &str, args: &[&str]) -> Result<Output, RunnerError> {
  Command::new(program)
    .args(args)
    .env_remove("TOOLU_RUNNER_TOKEN")
    .env_remove("TOOLU_RUNNER_CLIENT_ID")
    .output()
    .map_err(|e| RunnerError::Config(format!("failed to run `{program} {}`: {e}", args.join(" "))))
}

/// Run `program args`, requiring a zero exit; otherwise an error carrying the
/// command line, exit status, and captured stderr.
pub(crate) fn run_checked(program: &str, args: &[&str]) -> Result<(), RunnerError> {
  let output = run_capture(program, args)?;
  if output.status.success() {
    return Ok(());
  }
  Err(command_failed(program, args, &output))
}

/// Run `program args`, requiring a zero exit, and return trimmed stdout.
pub(crate) fn run_stdout(program: &str, args: &[&str]) -> Result<String, RunnerError> {
  let output = run_capture(program, args)?;
  if !output.status.success() {
    return Err(command_failed(program, args, &output));
  }
  Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// `true` if `program args` ran and exited zero; an IO failure or non-zero
/// exit is `false` (used for the modern-then-legacy launchctl fallback).
fn succeeds(program: &str, args: &[&str]) -> bool {
  run_capture(program, args).is_ok_and(|o| o.status.success())
}

/// Build the "command failed" error naming the command, status, and stderr.
fn command_failed(program: &str, args: &[&str], output: &Output) -> RunnerError {
  RunnerError::Config(format!(
    "`{program} {}` failed ({}): {}",
    args.join(" "),
    output.status,
    String::from_utf8_lossy(&output.stderr).trim()
  ))
}

#[cfg(test)]
#[path = "tests/service_cmd.rs"]
mod tests;
