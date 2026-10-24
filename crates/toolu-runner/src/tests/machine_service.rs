//! The unit inventory behind migration and `doctor` (#208), over real unit
//! files in temporary homes: text from the real renderers plus the static
//! `install.sh --service` units from `scripts/`. Both supervisors are
//! exercised on every host — the inventory only reads files.

use std::path::{Path, PathBuf};

use config::service_unit::{
  MachineSpec, ServiceSpec, Supervises, launchd_machine_plist, launchd_plist, systemd_machine_unit,
  systemd_unit,
};

use super::{Disposition, InstalledUnit, UnitScope, installed_units_for_scope};
use crate::service_cmd::Supervisor;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const STATIC_SYSTEMD: &str = include_str!("../../../../scripts/toolu-runner.service");
const STATIC_LAUNCHD: &str = include_str!("../../../../scripts/io.toolu-runner.plist");

/// A minimal real registration config (only its existence matters here).
fn register(config: &Path) -> std::io::Result<()> {
  std::fs::create_dir_all(config.parent().unwrap_or(config))?;
  std::fs::write(config, "runner_url = \"https://github.com/octo/a\"\n")
}

/// The per-registration unit text the bin renders for `config`.
fn per_registration(supervisor: Supervisor, label: &str, config: &Path) -> String {
  let spec = ServiceSpec {
    label,
    exe: Path::new("/usr/local/bin/toolu-runner"),
    config_path: config,
    diag_dir: Path::new("/tmp/_diag"),
    env_path: "/usr/bin:/bin",
    work_dir: Path::new("/tmp"),
  };
  match supervisor {
    Supervisor::Systemd => systemd_unit(&spec),
    Supervisor::Launchd => launchd_plist(&spec),
  }
}

/// The machine unit text the bin renders for runner home `home` (pinned).
fn machine(supervisor: Supervisor, home: &Path) -> String {
  let spec = MachineSpec {
    exe: Path::new("/usr/local/bin/toolu-runner"),
    home: Some(home),
    diag_dir: Path::new("/tmp/_diag"),
    env_path: "/usr/bin:/bin",
    work_dir: Path::new("/tmp"),
  };
  match supervisor {
    Supervisor::Systemd => systemd_machine_unit(&spec),
    Supervisor::Launchd => launchd_machine_plist(&spec),
  }
}

/// The unit named `file` in `units`.
fn named<'a>(units: &'a [InstalledUnit], file: &str) -> Option<&'a InstalledUnit> {
  units.iter().find(|unit| unit.id.unit == file)
}

/// A temp user home + runner home holding registrations `octo/a` and the
/// legacy root, and a config outside the runner home.
struct Homes {
  _dir: tempfile::TempDir,
  user: PathBuf,
  runner: PathBuf,
  repo: PathBuf,
  legacy: PathBuf,
  foreign: PathBuf,
}

fn homes() -> Result<Homes, Box<dyn std::error::Error>> {
  let dir = tempfile::tempdir()?;
  let user = dir.path().join("user");
  let runner = dir.path().join("runner-home");
  let repo = runner.join("runners/octo/a/config.toml");
  let legacy = runner.join("config.toml");
  let foreign = dir.path().join("elsewhere/config.toml");
  for config in [&repo, &legacy, &foreign] {
    register(config)?;
  }
  Ok(Homes {
    _dir: dir,
    user,
    runner,
    repo,
    legacy,
    foreign,
  })
}

#[test]
fn systemd_inventory_classifies_every_kind_of_old_unit() -> TestResult {
  let h = homes()?;
  let sd = Supervisor::Systemd;
  let dir = h.user.join(".config/systemd/user");
  std::fs::create_dir_all(&dir)?;
  let gone = h.runner.join("runners/octo/gone/config.toml");
  let files = [
    (
      "toolu-runner-octo-a.service",
      per_registration(sd, "io.toolu.runner.octo.a", &h.repo),
    ),
    (
      "toolu-runner-octo-gone.service",
      per_registration(sd, "io.toolu.runner.octo.gone", &gone),
    ),
    (
      "toolu-runner.service",
      per_registration(sd, "io.toolu.runner", &h.legacy),
    ),
    (
      "toolu-runner-foreign.service",
      per_registration(sd, "x", &h.foreign),
    ),
    (
      "toolu-runner-other.service",
      "[Service]\nExecStart=/usr/bin/true\n".to_owned(),
    ),
    ("nginx.service", per_registration(sd, "x", &h.repo)),
  ];
  for (file, text) in &files {
    std::fs::write(dir.join(file), text)?;
  }
  let system = h.user.join("etc-systemd-system");
  std::fs::create_dir_all(&system)?;
  std::fs::write(system.join("toolu-runner.service"), STATIC_SYSTEMD)?;

  let units = installed_units_for_scope(sd, UnitScope::User, &h.user, &h.runner, Some(&system));
  assert!(named(&units, "nginx.service").is_none(), "not a toolu unit");
  let disposition = |file: &str| named(&units, file).map(|unit| unit.disposition.clone());
  assert_eq!(
    disposition("toolu-runner-octo-a.service"),
    Some(Disposition::Replace(
      "the machine unit runs its registration"
    ))
  );
  assert_eq!(
    disposition("toolu-runner-octo-gone.service"),
    Some(Disposition::Replace("its config no longer exists"))
  );
  let user_legacy = units
    .iter()
    .find(|unit| unit.scope == UnitScope::User && unit.id.unit == "toolu-runner.service")
    .ok_or("no legacy-root unit")?;
  assert_eq!(
    user_legacy.disposition,
    Disposition::Replace("it occupies the machine unit's file")
  );
  assert!(matches!(
    disposition("toolu-runner-foreign.service"),
    Some(Disposition::Keep(why)) if why.contains("elsewhere/config.toml")
  ));
  assert_eq!(
    disposition("toolu-runner-other.service"),
    Some(Disposition::Keep(
      "it does not run `toolu-runner run`".to_owned()
    ))
  );

  // The static install.sh unit is system scope: its hard-coded config is
  // absent, and only a root command can remove it.
  let static_unit = units
    .iter()
    .find(|unit| unit.scope == UnitScope::System)
    .ok_or("the static system unit is not inventoried")?;
  assert_eq!(
    static_unit.supervises,
    Some(Supervises::Config(PathBuf::from(
      "/etc/toolu-runner/config.toml"
    )))
  );
  assert_eq!(
    static_unit.removal_command(sd),
    format!(
      "sudo systemctl disable --now toolu-runner.service && sudo rm {}",
      system.join("toolu-runner.service").display()
    )
  );
  Ok(())
}

#[test]
fn the_machine_unit_is_recognised_and_another_run_all_unit_is_replaced() -> TestResult {
  let h = homes()?;
  let sd = Supervisor::Systemd;
  let dir = h.user.join(".config/systemd/user");
  std::fs::create_dir_all(&dir)?;
  std::fs::write(dir.join("toolu-runner.service"), machine(sd, &h.runner))?;
  std::fs::write(
    dir.join("toolu-runner-copy.service"),
    machine(sd, &h.runner),
  )?;

  let units = installed_units_for_scope(sd, UnitScope::User, &h.user, &h.runner, None);
  let machine_unit = named(&units, "toolu-runner.service").ok_or("no machine unit")?;
  assert_eq!(machine_unit.disposition, Disposition::Machine);
  assert!(machine_unit.supervises_registration(&h.repo));
  assert!(machine_unit.supervises_registration(&h.legacy));
  assert_eq!(
    named(&units, "toolu-runner-copy.service").map(|unit| unit.disposition.clone()),
    Some(Disposition::Replace(
      "another `run --all` unit for this home"
    ))
  );
  Ok(())
}

#[test]
fn launchd_inventory_includes_the_static_install_sh_agent() -> TestResult {
  let h = homes()?;
  let ld = Supervisor::Launchd;
  let dir = h.user.join("Library/LaunchAgents");
  std::fs::create_dir_all(&dir)?;
  std::fs::write(
    dir.join("io.toolu.runner.octo.a.plist"),
    per_registration(ld, "io.toolu.runner.octo.a", &h.repo),
  )?;
  std::fs::write(
    dir.join("io.toolu.runner.plist"),
    per_registration(ld, "io.toolu.runner", &h.legacy),
  )?;
  std::fs::write(dir.join("io.toolu-runner.plist"), STATIC_LAUNCHD)?;
  std::fs::write(dir.join("sh.toolu.runner.plist"), machine(ld, &h.runner))?;
  std::fs::write(dir.join("com.example.other.plist"), machine(ld, &h.runner))?;

  let units = installed_units_for_scope(ld, UnitScope::User, &h.user, &h.runner, None);
  let by_label = |label: &str| units.iter().find(|unit| unit.id.label == label);
  assert!(by_label("com.example.other").is_none(), "not a toolu agent");
  assert_eq!(
    by_label("sh.toolu.runner").map(|unit| unit.disposition.clone()),
    Some(Disposition::Machine)
  );
  for label in ["io.toolu.runner.octo.a", "io.toolu.runner"] {
    assert_eq!(
      by_label(label).map(|unit| unit.disposition.clone()),
      Some(Disposition::Replace(
        "the machine unit runs its registration"
      )),
      "{label}"
    );
  }
  // `/Users/Shared/toolu-runner/config.toml`, hard-coded by install.sh, is
  // never written by `register`.
  let static_agent = by_label("io.toolu-runner").ok_or("static agent missing")?;
  if !Path::new("/Users/Shared/toolu-runner/config.toml").exists() {
    assert_eq!(
      static_agent.disposition,
      Disposition::Replace("its config no longer exists")
    );
  }
  assert_eq!(
    static_agent.removal_command(ld),
    format!(
      "launchctl bootout gui/$(id -u)/io.toolu-runner && rm {}",
      dir.join("io.toolu-runner.plist").display()
    )
  );
  Ok(())
}

#[test]
fn a_per_registration_unit_supervises_only_its_own_registration() -> TestResult {
  let h = homes()?;
  let sd = Supervisor::Systemd;
  let dir = h.user.join(".config/systemd/user");
  std::fs::create_dir_all(&dir)?;
  std::fs::write(
    dir.join("toolu-runner-octo-a.service"),
    per_registration(sd, "io.toolu.runner.octo.a", &h.repo),
  )?;
  let units = installed_units_for_scope(sd, UnitScope::User, &h.user, &h.runner, None);
  let unit = named(&units, "toolu-runner-octo-a.service").ok_or("unit missing")?;
  assert!(unit.supervises_registration(&h.repo));
  assert!(!unit.supervises_registration(&h.legacy));
  Ok(())
}

/// A `run --all` unit pinned to another runner home (`/srv/tr-ci` vs
/// `/srv/tr`: a prefix, not a parent) serves only that home: at the machine
/// path it is never treated as this home's machine unit, elsewhere it is
/// kept, and it supervises only that home's registrations.
#[test]
fn a_machine_unit_pinned_to_another_home_is_not_this_homes() -> TestResult {
  let dir = tempfile::tempdir()?;
  let user = dir.path().join("user");
  let units_dir = user.join(".config/systemd/user");
  std::fs::create_dir_all(&units_dir)?;
  let other = dir.path().join("srv/tr-ci");
  let this = dir.path().join("srv/tr");
  let pinned = |home: &Path| {
    systemd_machine_unit(&MachineSpec {
      exe: Path::new("/usr/local/bin/toolu-runner"),
      home: Some(home),
      diag_dir: Path::new("/tmp/_diag"),
      env_path: "/usr/bin",
      work_dir: home,
    })
  };
  std::fs::write(units_dir.join("toolu-runner.service"), pinned(&other))?;
  std::fs::write(units_dir.join("toolu-runner-ci.service"), pinned(&other))?;
  std::fs::write(units_dir.join("toolu-runner-mine.service"), pinned(&this))?;

  let units = installed_units_for_scope(Supervisor::Systemd, UnitScope::User, &user, &this, None);
  for file in ["toolu-runner.service", "toolu-runner-ci.service"] {
    let unit = named(&units, file).ok_or(file)?;
    assert_eq!(
      unit.disposition,
      Disposition::OtherHome(other.clone()),
      "{file}"
    );
    assert!(unit.supervises_registration(&other.join("runners/o/r/config.toml")));
    assert!(!unit.supervises_registration(&this.join("runners/o/r/config.toml")));
  }
  assert_eq!(
    named(&units, "toolu-runner-mine.service").map(|unit| unit.disposition.clone()),
    Some(Disposition::Replace(
      "another `run --all` unit for this home"
    ))
  );
  Ok(())
}

#[test]
fn root_inventory_scans_each_scope_once_and_protects_foreign_system_unit() -> TestResult {
  let h = homes()?;
  let sd = Supervisor::Systemd;
  let user_dir = h.user.join(".config/systemd/user");
  let system_dir = h.user.join("etc-systemd-system");
  std::fs::create_dir_all(&user_dir)?;
  std::fs::create_dir_all(&system_dir)?;
  std::fs::write(
    user_dir.join("toolu-runner-octo-a.service"),
    per_registration(sd, "io.toolu.runner.octo.a", &h.repo),
  )?;
  let machine_path = system_dir.join("toolu-runner.service");
  std::fs::write(&machine_path, per_registration(sd, "foreign", &h.foreign))?;
  let units =
    installed_units_for_scope(sd, UnitScope::System, &h.user, &h.runner, Some(&system_dir));
  assert_eq!(units.len(), 2, "each directory should be scanned once");
  let foreign = units
    .iter()
    .find(|unit| unit.path == machine_path)
    .ok_or("system unit missing")?;
  assert!(matches!(&foreign.disposition, Disposition::Keep(why) if why.contains("elsewhere")));
  assert!(super::refuse_foreign_at(&machine_path, &units).is_err());
  assert_eq!(
    named(&units, "toolu-runner-octo-a.service").map(|unit| unit.disposition.clone()),
    Some(Disposition::Replace(
      "the machine unit runs its registration"
    ))
  );

  std::fs::write(&machine_path, per_registration(sd, "mine", &h.repo))?;
  let units =
    installed_units_for_scope(sd, UnitScope::System, &h.user, &h.runner, Some(&system_dir));
  let mine = units
    .iter()
    .find(|unit| unit.path == machine_path)
    .ok_or("system unit missing")?;
  assert_eq!(
    mine.disposition,
    Disposition::Replace("the machine unit runs its registration")
  );
  assert!(super::refuse_foreign_at(&machine_path, &units).is_ok());
  Ok(())
}

#[cfg(unix)]
#[test]
fn root_inventory_sees_a_masked_machine_unit_symlink() -> TestResult {
  let h = homes()?;
  let system_dir = h.user.join("etc-systemd-system");
  std::fs::create_dir_all(&system_dir)?;
  let masked = system_dir.join("toolu-runner.service");
  std::os::unix::fs::symlink("/dev/null", &masked)?;
  let units = installed_units_for_scope(
    Supervisor::Systemd,
    UnitScope::System,
    &h.user,
    &h.runner,
    Some(&system_dir),
  );
  assert!(units.iter().any(|unit| unit.path == masked));
  assert!(super::refuse_foreign_at(&masked, &units).is_err());
  Ok(())
}
