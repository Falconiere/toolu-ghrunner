//! The machine unit (#208): one launchd agent (`sh.toolu.runner`) or systemd
//! user unit (`toolu-runner.service`) that runs `run --all` for every
//! registration. Owns `install-service` without `--config` (migrating older
//! toolu units first), the inventory of installed units that `doctor`
//! shares, and the uninstall after `remove` deletes the last registration.

use std::path::{Path, PathBuf};

use config::registry;
use config::service_unit::{
  MACHINE_LABEL, MACHINE_UNIT, MachineSpec, ServiceAction, Supervises, launchd_env_path,
  launchd_machine_plist, parse_launchd_home, parse_launchd_target, parse_systemd_home,
  parse_systemd_target, service_action_summary, systemd_machine_unit, systemd_system_machine_unit,
};
use shared::RunnerError;

use crate::cli::InstallServiceArgs;
use crate::service_cmd::{self, ServiceId, Supervisor};

pub(crate) use crate::service_cmd::UnitScope;

/// Where `install.sh --service` puts its static systemd unit (system scope).
pub(crate) const SYSTEM_UNIT_DIR: &str = "/etc/systemd/system";

/// The machine unit's identity.
pub(crate) fn machine_id() -> ServiceId {
  ServiceId {
    label: MACHINE_LABEL.to_owned(),
    unit: MACHINE_UNIT.to_owned(),
  }
}

/// What migration does with an installed toolu unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Disposition {
  /// The machine unit itself.
  Machine,
  /// Deactivated and deleted before the machine unit starts, and why.
  Replace(&'static str),
  /// Left running, and why: it runs something the machine unit does not.
  Keep(String),
  /// A `run --all` unit serving another runner home (named): kept, and the
  /// install refuses to overwrite it when it sits at the machine path.
  OtherHome(PathBuf),
}

/// One installed toolu supervisor unit.
#[derive(Debug, Clone)]
pub(crate) struct InstalledUnit {
  /// The unit file.
  pub(crate) path: PathBuf,
  /// Its manager scope.
  pub(crate) scope: UnitScope,
  /// launchd label (the plist stem) and systemd unit (the file name).
  pub(crate) id: ServiceId,
  /// What its command line runs, when it is a toolu `run`.
  pub(crate) supervises: Option<Supervises>,
  /// For a `run --all` unit, the runner home it serves: its pinned
  /// `TOOLU_RUNNER_HOME`, else the default `~/.toolu-runner`.
  pub(crate) serves: Option<PathBuf>,
  /// What migration does with it.
  pub(crate) disposition: Disposition,
}

impl InstalledUnit {
  /// Whether this unit drives the registration at `config_path`: a
  /// user-scope `run --all` unit for the registration's runner home, or a
  /// `run --config` unit for that very config.
  pub(crate) fn supervises_registration(&self, config_path: &Path) -> bool {
    match &self.supervises {
      Some(Supervises::All) => self
        .serves
        .as_deref()
        .is_some_and(|home| within(config_path, home)),
      Some(Supervises::Config(path)) => same_file(path, config_path),
      None => false,
    }
  }

  /// The command that deactivates and deletes this unit by hand.
  pub(crate) fn removal_command(&self, supervisor: Supervisor) -> String {
    let path = self.path.display();
    match (supervisor, self.scope) {
      (Supervisor::Systemd, UnitScope::System) => format!(
        "sudo systemctl disable --now {} && sudo rm {path}",
        self.id.unit
      ),
      (Supervisor::Systemd, UnitScope::User) => format!(
        "systemctl --user disable --now {} && rm {path}",
        self.id.unit
      ),
      (Supervisor::Launchd, _) => format!(
        "launchctl bootout gui/$(id -u)/{} && rm {path}",
        self.id.label
      ),
    }
  }
}

/// Inventory with the destination scope selected for this installation.
pub(crate) fn installed_units_for_scope(
  supervisor: Supervisor,
  target_scope: UnitScope,
  user_home: &Path,
  runner_home: &Path,
  system_dir: Option<&Path>,
) -> Vec<InstalledUnit> {
  let machine_dest =
    service_cmd::dest_path_scoped(supervisor, target_scope, &machine_id(), user_home);
  // Unreadable registry: no unit counts as running a home registration, so
  // nothing is replaced on that ground (units are kept, never deleted);
  // `install-service` refuses before reaching here.
  let registrations: Vec<PathBuf> = match registry::list_registrations(runner_home) {
    Ok(entries) => entries.into_iter().map(|entry| entry.config_path).collect(),
    Err(e) => {
      tracing::warn!(error = %e, "cannot list registrations; no unit is classified as running one");
      Vec::new()
    },
  };
  let user_dest = service_cmd::dest_path(supervisor, &machine_id(), user_home);
  let mut units: Vec<InstalledUnit> = user_dest
    .parent()
    .map(|dir| unit_files(supervisor, dir))
    .unwrap_or_default()
    .into_iter()
    .map(|path| inspect(supervisor, path, UnitScope::User))
    .collect();
  if supervisor == Supervisor::Systemd
    && let Some(dir) = system_dir
  {
    units.extend(
      unit_files(supervisor, dir)
        .into_iter()
        .map(|path| inspect(supervisor, path, UnitScope::System)),
    );
  }
  for unit in &mut units {
    unit.disposition = disposition(
      unit,
      &machine_dest,
      target_scope,
      &registrations,
      runner_home,
    );
  }
  units
}

/// The toolu unit files in a user unit dir, sorted.
fn unit_files(supervisor: Supervisor, dir: &Path) -> Vec<PathBuf> {
  let entries = match std::fs::read_dir(dir) {
    Ok(entries) => entries,
    // No unit dir yet: no units, the common case.
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
    Err(e) => {
      tracing::warn!(dir = %dir.display(), error = %e, "cannot list the unit dir; its units are not inventoried");
      return Vec::new();
    },
  };
  let mut files: Vec<PathBuf> = entries
    .flatten()
    .map(|entry| entry.path())
    .filter(|path| {
      is_toolu_unit(supervisor, path)
        && std::fs::symlink_metadata(path)
          .is_ok_and(|metadata| metadata.is_file() || metadata.file_type().is_symlink())
    })
    .collect();
  files.sort();
  files
}

/// systemd `toolu-runner*.service`; launchd `io.toolu.runner[.*].plist`,
/// the static `io.toolu-runner.plist`, and `sh.toolu.runner.plist`.
fn is_toolu_unit(supervisor: Supervisor, path: &Path) -> bool {
  let name = path
    .file_name()
    .map(|name| name.to_string_lossy())
    .unwrap_or_default();
  match supervisor {
    Supervisor::Systemd => name.starts_with("toolu-runner") && name.ends_with(".service"),
    Supervisor::Launchd => name.strip_suffix(".plist").is_some_and(|stem| {
      stem == "io.toolu.runner"
        || stem.starts_with("io.toolu.runner.")
        || stem == "io.toolu-runner"
        || stem == MACHINE_LABEL
    }),
  }
}

/// Read one unit file: its identity, what it runs, and the home a
/// `run --all` unit serves.
fn inspect(supervisor: Supervisor, path: PathBuf, scope: UnitScope) -> InstalledUnit {
  // Unreadable: it parses to nothing and is kept, never deleted — but say so.
  let text = std::fs::read_to_string(&path).unwrap_or_else(|e| {
    tracing::warn!(path = %path.display(), error = %e, "cannot read the unit; leaving it alone");
    String::new()
  });
  let (supervises, pinned) = match supervisor {
    Supervisor::Systemd => (parse_systemd_target(&text), parse_systemd_home(&text)),
    Supervisor::Launchd => (parse_launchd_target(&text), parse_launchd_home(&text)),
  };
  let serves = matches!(supervises, Some(Supervises::All))
    .then(|| pinned.unwrap_or_else(registry::default_runner_home));
  let file = path
    .file_name()
    .map(|name| name.to_string_lossy().into_owned())
    .unwrap_or_default();
  let id = ServiceId {
    label: file.trim_end_matches(".plist").to_owned(),
    unit: file,
  };
  InstalledUnit {
    path,
    scope,
    id,
    supervises,
    serves,
    disposition: Disposition::Machine,
  }
}

/// Migration rule (Jev-checked, see the #208 design): the machine unit's own
/// file is replaced unless it already is the machine unit; a unit is replaced
/// when it runs a registration under `runner_home`, a config that no longer
/// exists, or another `run --all` for this home; a `run --all` for another
/// home and anything else keep running.
fn disposition(
  unit: &InstalledUnit,
  machine_dest: &Path,
  target_scope: UnitScope,
  registrations: &[PathBuf],
  runner_home: &Path,
) -> Disposition {
  let at_machine_path = unit.scope == target_scope && unit.path == machine_dest;
  if let Some(home) = &unit.serves
    && !same_file(home, runner_home)
  {
    return Disposition::OtherHome(home.clone());
  }
  match &unit.supervises {
    Some(Supervises::All) if at_machine_path => Disposition::Machine,
    Some(Supervises::All) => Disposition::Replace("another `run --all` unit for this home"),
    Some(Supervises::Config(config))
      if at_machine_path
        && target_scope == UnitScope::System
        && config.exists()
        && !registrations.iter().any(|known| same_file(known, config)) =>
    {
      Disposition::Keep(format!(
        "it runs {}, which is not a registration under {}",
        config.display(),
        runner_home.display()
      ))
    },
    None if at_machine_path && target_scope == UnitScope::System => {
      Disposition::Keep("it does not run `toolu-runner run`".to_owned())
    },
    _ if at_machine_path => Disposition::Replace("it occupies the machine unit's file"),
    Some(Supervises::Config(config)) if !config.exists() => {
      Disposition::Replace("its config no longer exists")
    },
    Some(Supervises::Config(config))
      if registrations.iter().any(|known| same_file(known, config)) =>
    {
      Disposition::Replace("the machine unit runs its registration")
    },
    Some(Supervises::Config(config)) => Disposition::Keep(format!(
      "it runs {}, which is not a registration under {}",
      config.display(),
      runner_home.display()
    )),
    None => Disposition::Keep("it does not run `toolu-runner run`".to_owned()),
  }
}

/// Whether `path` lies under `dir` (canonicalized when both exist).
fn within(path: &Path, dir: &Path) -> bool {
  match (path.canonicalize(), dir.canonicalize()) {
    (Ok(path), Ok(dir)) => path.starts_with(dir),
    _ => path.starts_with(dir),
  }
}

/// Whether two paths name the same file (canonicalized when both exist).
fn same_file(a: &Path, b: &Path) -> bool {
  match (a.canonicalize(), b.canonicalize()) {
    (Ok(a), Ok(b)) => a == b,
    _ => a == b,
  }
}

/// `install-service` without `--config`: print, remove, or migrate + write
/// (+ activate) the machine unit.
pub(crate) fn cmd_install_machine(
  args: &InstallServiceArgs,
) -> Result<(), Box<dyn std::error::Error>> {
  let supervisor = service_cmd::current_supervisor()?;
  let scope = service_cmd::current_unit_scope(supervisor)?;
  let runner_home = std::path::absolute(registry::runner_home())?;
  let id = machine_id();
  if args.print {
    print!("{}", render(supervisor, scope, &runner_home)?);
    if let Ok(user_home) = service_cmd::home_dir() {
      report_plan(supervisor, scope, &user_home, &runner_home);
    }
    return Ok(());
  }
  let user_home = service_cmd::home_dir()?;
  let dest = service_cmd::dest_path_scoped(supervisor, scope, &id, &user_home);
  if args.remove {
    let outcome = service_cmd::remove_service(supervisor, scope, &dest, &id)?;
    println!(
      "{}",
      service_action_summary(&outcome.action, &outcome.label, &outcome.path)
    );
    return Ok(());
  }
  // Migration decides from the registrations: never on a partial listing.
  registry::list_registrations(&runner_home)?;
  let units = installed_units_for_scope(
    supervisor,
    scope,
    &user_home,
    &runner_home,
    Some(Path::new(SYSTEM_UNIT_DIR)),
  );
  refuse_foreign_at(&dest, &units)?;
  let unit_text = render(supervisor, scope, &runner_home)?;
  let linger = service_cmd::service_linger::before_install(supervisor, scope, args)?;
  if args.no_activate {
    write_only(supervisor, scope, &dest, &runner_home, &unit_text, &units)?;
    service_cmd::service_linger::warn_if_short_lived(linger);
    return Ok(());
  }
  activate_machine_install(supervisor, scope, &runner_home, &dest, &unit_text, &units)?;
  service_cmd::service_linger::warn_if_short_lived(linger);
  Ok(())
}

fn activate_machine_install(
  supervisor: Supervisor,
  scope: UnitScope,
  runner_home: &Path,
  dest: &Path,
  unit_text: &str,
  units: &[InstalledUnit],
) -> Result<(), Box<dyn std::error::Error>> {
  replace_old_units(supervisor, scope, units)?;
  report_kept(supervisor, scope, units);
  registry::ensure_runner_home(runner_home)?;
  std::fs::create_dir_all(runner_home.join("_diag"))?;
  service_cmd::write_unit(dest, unit_text)?;
  let id = machine_id();
  service_cmd::activate(supervisor, scope, dest, &id)?;
  let installed = ServiceAction::Installed { activated: true };
  println!("{}", service_action_summary(&installed, &id.label, dest));
  // A re-run over a running supervisor: have it rescan now.
  if let Err(e) = crate::supervisor_signal::notify(runner_home) {
    eprintln!("warning: could not notify the running service: {e}");
  }
  Ok(())
}

/// `--no-activate`: write the machine unit only; old units stay as they are.
fn write_only(
  supervisor: Supervisor,
  scope: UnitScope,
  dest: &Path,
  runner_home: &Path,
  unit_text: &str,
  units: &[InstalledUnit],
) -> Result<(), Box<dyn std::error::Error>> {
  registry::ensure_runner_home(runner_home)?;
  std::fs::create_dir_all(runner_home.join("_diag"))?;
  service_cmd::write_unit(dest, unit_text)?;
  println!("wrote {}", dest.display());
  println!(
    "activate it with:\n  {}",
    service_cmd::activation_hint(supervisor, scope, dest, &machine_id())
  );
  for unit in units {
    if let Disposition::Replace(why) = &unit.disposition {
      println!(
        "still installed: {} ({why}) — `toolu-runner install-service` replaces it",
        unit.path.display()
      );
    }
  }
  report_kept(supervisor, scope, units);
  Ok(())
}

/// Refuse to overwrite a unit at the machine path when it serves a different
/// live home or config. A root system unit may already occupy that path.
fn refuse_foreign_at(dest: &Path, units: &[InstalledUnit]) -> Result<(), RunnerError> {
  if std::fs::symlink_metadata(dest).is_ok_and(|meta| meta.file_type().is_symlink()) {
    return Err(RunnerError::Config(format!(
      "the machine service at {} is a symlink; remove it before installing so its target is not overwritten",
      dest.display()
    )));
  }
  let foreign = units
    .iter()
    .filter(|unit| unit.path == dest)
    .find_map(|unit| match &unit.disposition {
      Disposition::OtherHome(home) => Some(format!("another runner home {}", home.display())),
      Disposition::Keep(why) => Some(why.clone()),
      Disposition::Machine | Disposition::Replace(_) => None,
    });
  match foreign {
    Some(why) => Err(RunnerError::Config(format!(
      "the machine service at {} serves {why}; it was kept and nothing changed — \
       remove it first or supervise this home's registrations with \
       `install-service --config <path>`",
      dest.display(),
    ))),
    None => Ok(()),
  }
}

/// Deactivate, then delete, every user-scope unit migration replaces —
/// before the machine unit starts. Stopping a unit stops its runner like
/// `systemctl stop` does, so a job it is running is cancelled (said per
/// unit). A unit that cannot be deactivated keeps its file and aborts the
/// install, so a running process is never left without its unit.
fn replace_old_units(
  supervisor: Supervisor,
  target_scope: UnitScope,
  units: &[InstalledUnit],
) -> Result<(), RunnerError> {
  for unit in units {
    let Disposition::Replace(why) = &unit.disposition else {
      continue;
    };
    if unit.scope == UnitScope::System && target_scope != UnitScope::System {
      continue;
    }
    println!(
      "stopping {} ({why}); a job it is running is cancelled",
      unit.path.display()
    );
    service_cmd::deactivate_checked(supervisor, unit.scope, &unit.path, &unit.id).map_err(|e| {
      RunnerError::Config(format!(
        "could not deactivate {}: {e}; it was kept and nothing else changed — fix the \
         supervisor connection and re-run, or remove it by hand: {}",
        unit.path.display(),
        unit.removal_command(supervisor)
      ))
    })?;
    match std::fs::remove_file(&unit.path) {
      Ok(()) => {},
      Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
      Err(e) => return Err(e.into()),
    }
    println!("replaced {} ({why})", unit.path.display());
  }
  Ok(())
}

/// Warn about each unit left running, with the command that removes it.
fn report_kept(supervisor: Supervisor, target_scope: UnitScope, units: &[InstalledUnit]) {
  for unit in units {
    let why = match (&unit.disposition, unit.scope) {
      (Disposition::Keep(why), _) => why.clone(),
      (Disposition::OtherHome(home), _) => format!("it runs the runner home {}", home.display()),
      (Disposition::Replace(why), UnitScope::System) if target_scope == UnitScope::User => {
        format!("{why}; it is a system unit owned by root")
      },
      _ => continue,
    };
    eprintln!(
      "warning: left {} running: {why} — remove it with: {}",
      unit.path.display(),
      unit.removal_command(supervisor)
    );
  }
}

/// `--print`: what a default install would do, on stderr.
fn report_plan(supervisor: Supervisor, scope: UnitScope, user_home: &Path, runner_home: &Path) {
  let units = installed_units_for_scope(
    supervisor,
    scope,
    user_home,
    runner_home,
    Some(Path::new(SYSTEM_UNIT_DIR)),
  );
  for unit in &units {
    if let Disposition::Replace(why) = &unit.disposition
      && (unit.scope == UnitScope::User || scope == UnitScope::System)
    {
      eprintln!("would replace {} ({why})", unit.path.display());
    }
  }
  report_kept(supervisor, scope, &units);
}

/// The machine unit text for this binary and `runner_home` (absolute).
/// System units also pin HOME and the default runner home.
fn render(
  supervisor: Supervisor,
  scope: UnitScope,
  runner_home: &Path,
) -> Result<String, RunnerError> {
  let exe = std::env::current_exe()
    .map_err(|e| RunnerError::Config(format!("cannot resolve the running binary's path: {e}")))?;
  let diag_dir = runner_home.join("_diag");
  let env_path = launchd_env_path(crate::config::path().as_deref());
  let pin_runner_home = registry::runner_home_is_custom() || scope == UnitScope::System;
  if supervisor == Supervisor::Systemd {
    service_cmd::validate_unit_environment("PATH", &env_path)?;
    if pin_runner_home {
      service_cmd::validate_path_environment("TOOLU_RUNNER_HOME", runner_home)?;
    }
  }
  let spec = MachineSpec {
    exe: &exe,
    home: pin_runner_home.then_some(runner_home),
    diag_dir: &diag_dir,
    env_path: &env_path,
    work_dir: runner_home,
  };
  Ok(match supervisor {
    Supervisor::Launchd => launchd_machine_plist(&spec),
    Supervisor::Systemd if scope == UnitScope::System => {
      let user_home = service_cmd::home_dir()?;
      service_cmd::validate_path_environment("HOME", &user_home)?;
      systemd_system_machine_unit(&spec, &user_home)
    },
    Supervisor::Systemd => systemd_machine_unit(&spec),
  })
}

/// After `remove` deleted the last registration under `runner_home`:
/// deactivate and delete the machine unit serving it. `Ok(Some(path))` when
/// a unit was removed; `Ok(None)` when registrations remain, no machine unit
/// serves this home, or this host has no supported user supervisor.
///
/// # Errors
///
/// The registry or the unit file could not be read or removed.
pub(crate) fn uninstall_if_last(runner_home: &Path) -> Result<Option<PathBuf>, RunnerError> {
  if !registry::list_registrations(runner_home)?.is_empty() {
    return Ok(None);
  }
  let (Ok(supervisor), Ok(user_home)) =
    (service_cmd::current_supervisor(), service_cmd::home_dir())
  else {
    return Ok(None);
  };
  let scope = service_cmd::current_unit_scope(supervisor)?;
  let dest = service_cmd::dest_path_scoped(supervisor, scope, &machine_id(), &user_home);
  if !dest.is_file() {
    return Ok(None);
  }
  let runner_home = std::path::absolute(runner_home)?;
  let unit = inspect(supervisor, dest.clone(), scope);
  if !unit
    .serves
    .as_deref()
    .is_some_and(|home| same_file(home, &runner_home))
  {
    return Ok(None);
  }
  let outcome = service_cmd::remove_service(supervisor, scope, &dest, &machine_id())?;
  Ok((outcome.action == ServiceAction::Removed).then_some(dest))
}

#[cfg(test)]
#[path = "tests/machine_service.rs"]
mod tests;
