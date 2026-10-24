//! `install-service` without `--config` (#208): the machine unit through the
//! real binary in a temporary HOME (both `HOME` and `TOOLU_RUNNER_HOME`).
//!
//! Old units are real unit text: written by a real `install-service --config
//! … --no-activate`, rendered by the real renderers, or the static
//! `install.sh` units from `scripts/`. Activation goes through PATH-stub
//! `systemctl` / `launchctl` that log their argv — a real user manager reads
//! units from its own config dirs, not a temporary HOME, and loading one
//! would start a real service on the test host (see the #208 design).
//!
//! - AC-1: `--print` shows exactly the rendered machine unit, writes nothing.
//! - AC-7: migration deactivates every old unit of this home (per-registration,
//!   orphaned, legacy root at the machine unit's path, the static agent)
//!   before starting the machine unit; a unit running a config outside the
//!   home is kept with its removal command.
//! - AC-13: `doctor` fails a registration two units supervise.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use config::service_unit::{
  self, MachineSpec, ServiceSpec, Supervises, launchd_env_path, parse_launchd_target,
  parse_systemd_target,
};

use crate::install_service_test::{real_uid, shim_env, write_fixture, write_shim};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const MACOS: bool = cfg!(target_os = "macos");
/// A stub supervisor CLI: log argv, succeed.
const RECORD_OK: &str = "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$SHIM_LOG\"\nexit 0\n";

/// `toolu-runner <args>` pinned to `home`.
fn toolu(home: &Path, args: &[&str]) -> Command {
  let mut cmd = Command::new(env!("CARGO_BIN_EXE_toolu-runner"));
  cmd
    .env("HOME", home)
    .env("TOOLU_RUNNER_HOME", home)
    .env("TOOLU_RUNNER_NO_KEYRING", "1")
    .args(args);
  cmd
}

/// Run `toolu-runner <args>` with the stub supervisor on `PATH`; returns the
/// output and the stub's argv log.
fn with_stub(home: &Path, args: &[&str]) -> Result<(Output, String), Box<dyn std::error::Error>> {
  let (bin, log, path_env) = shim_env(home)?;
  write_shim(
    &bin,
    if MACOS { "launchctl" } else { "systemctl" },
    RECORD_OK,
  )?;
  let output = toolu(home, args)
    .env("PATH", &path_env)
    .env("SHIM_LOG", &log)
    .output()?;
  Ok((output, std::fs::read_to_string(&log).unwrap_or_default()))
}

/// The user unit dir under `home`.
fn unit_dir(home: &Path) -> PathBuf {
  if MACOS {
    home.join("Library/LaunchAgents")
  } else {
    home.join(".config/systemd/user")
  }
}

/// The machine unit file under `home`.
fn machine_dest(home: &Path) -> PathBuf {
  unit_dir(home).join(if MACOS {
    "sh.toolu.runner.plist"
  } else {
    "toolu-runner.service"
  })
}

/// What a unit file runs.
fn target(path: &Path) -> Option<Supervises> {
  let text = std::fs::read_to_string(path).ok()?;
  if MACOS {
    parse_launchd_target(&text)
  } else {
    parse_systemd_target(&text)
  }
}

/// A per-registration unit as an older `install-service` rendered it.
fn write_old_unit(home: &Path, file: &str, label: &str, config: &Path) -> std::io::Result<PathBuf> {
  let spec = ServiceSpec {
    label,
    exe: Path::new("/usr/local/bin/toolu-runner"),
    config_path: config,
    diag_dir: Path::new("/tmp/_diag"),
    env_path: "/usr/bin:/bin",
    work_dir: Path::new("/tmp"),
  };
  let text = if MACOS {
    service_unit::launchd_plist(&spec)
  } else {
    service_unit::systemd_unit(&spec)
  };
  let path = unit_dir(home).join(file);
  std::fs::create_dir_all(unit_dir(home))?;
  std::fs::write(&path, text)?;
  Ok(path)
}

/// A per-platform unit file name: `(systemd, launchd)`.
fn unit_file(systemd: &str, launchd: &str) -> String {
  if MACOS { launchd } else { systemd }.to_owned()
}

/// The legacy-root registration `<home>/config.toml` (a copy of a real one).
fn write_legacy(home: &Path, from: &Path) -> std::io::Result<PathBuf> {
  let legacy = home.join("config.toml");
  std::fs::copy(from, &legacy)?;
  std::fs::copy(
    from.with_file_name("credentials.json"),
    home.join("credentials.json"),
  )?;
  Ok(legacy)
}

fn text(bytes: &[u8]) -> String {
  String::from_utf8_lossy(bytes).into_owned()
}

/// AC-1.
#[test]
fn print_shows_the_machine_unit_and_writes_nothing() -> TestResult {
  let home = tempfile::tempdir()?;
  write_fixture(home.path(), "octo", "demo")?;
  let out = toolu(home.path(), &["install-service", "--print"]).output()?;
  assert!(out.status.success(), "{}", text(&out.stderr));

  let diag = home.path().join("_diag");
  let env_path = launchd_env_path(std::env::var("PATH").ok().as_deref());
  let spec = MachineSpec {
    exe: Path::new(env!("CARGO_BIN_EXE_toolu-runner")),
    home: Some(home.path()),
    diag_dir: &diag,
    env_path: &env_path,
    work_dir: home.path(),
  };
  let expected = if MACOS {
    service_unit::launchd_machine_plist(&spec)
  } else {
    service_unit::systemd_machine_unit(&spec)
  };
  assert_eq!(text(&out.stdout), expected);
  assert!(
    text(&out.stdout).contains("run --all") || text(&out.stdout).contains("<string>--all</string>")
  );
  assert!(
    !machine_dest(home.path()).exists(),
    "--print writes nothing"
  );
  Ok(())
}

/// AC-7.
#[test]
fn default_mode_migrates_old_units_then_starts_the_machine_unit() -> TestResult {
  let home = tempfile::tempdir()?;
  let elsewhere = tempfile::tempdir()?;
  let demo = write_fixture(home.path(), "octo", "demo")?;
  let legacy = write_legacy(home.path(), &demo)?;
  let foreign_config = write_fixture(elsewhere.path(), "far", "away")?;

  // A per-registration unit, written by the real CLI.
  let made = toolu(
    home.path(),
    &["install-service", "--no-activate", "--config"],
  )
  .arg(&demo)
  .output()?;
  assert!(made.status.success(), "{}", text(&made.stderr));
  let per_registration = unit_dir(home.path()).join(unit_file(
    "toolu-runner-octo-demo.service",
    "io.toolu.runner.octo.demo.plist",
  ));
  assert!(per_registration.is_file());
  let gone = home.path().join("runners/octo/gone/config.toml");
  let orphan = write_old_unit(
    home.path(),
    &unit_file(
      "toolu-runner-octo-gone.service",
      "io.toolu.runner.octo.gone.plist",
    ),
    "io.toolu.runner.octo.gone",
    &gone,
  )?;
  // The pre-#208 legacy-root unit: on systemd it sits at the machine path.
  let legacy_unit = write_old_unit(
    home.path(),
    &unit_file("toolu-runner.service", "io.toolu.runner.plist"),
    "io.toolu.runner",
    &legacy,
  )?;
  let foreign = write_old_unit(
    home.path(),
    &unit_file(
      "toolu-runner-foreign.service",
      "io.toolu.runner.foreign.plist",
    ),
    "io.toolu.runner.foreign",
    &foreign_config,
  )?;
  let static_agent = unit_dir(home.path()).join("io.toolu-runner.plist");
  if MACOS {
    std::fs::write(
      &static_agent,
      include_str!("../../../scripts/io.toolu-runner.plist"),
    )?;
  }

  let (out, calls) = with_stub(home.path(), &["install-service"])?;
  let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
  assert!(out.status.success(), "{stdout}{stderr}");

  // Exactly one unit runs this home's registrations: the machine unit.
  let dest = machine_dest(home.path());
  assert_eq!(target(&dest), Some(Supervises::All));
  let mut left: Vec<PathBuf> = std::fs::read_dir(unit_dir(home.path()))?
    .flatten()
    .map(|entry| entry.path())
    .collect();
  left.sort();
  let mut expected = vec![dest.clone(), foreign.clone()];
  expected.sort();
  assert_eq!(left, expected, "{stdout}{stderr}");
  if !MACOS {
    assert_eq!(
      legacy_unit, dest,
      "the legacy-root unit occupied the machine path"
    );
  }

  // Every old unit was deactivated before the machine unit started.
  let lines: Vec<&str> = calls.lines().collect();
  let start = if MACOS {
    format!("bootstrap gui/{} {}", real_uid()?, dest.display())
  } else {
    "--user enable --now toolu-runner.service".to_owned()
  };
  let started = lines
    .iter()
    .position(|line| *line == start)
    .ok_or(format!("never started:\n{calls}"))?;
  let mut stops: Vec<String> = Vec::new();
  if MACOS {
    let uid = real_uid()?;
    for label in [
      "io.toolu.runner.octo.demo",
      "io.toolu.runner.octo.gone",
      "io.toolu.runner",
      "io.toolu-runner",
    ] {
      stops.push(format!("bootout gui/{uid}/{label}"));
    }
  } else {
    for unit in [
      "toolu-runner-octo-demo.service",
      "toolu-runner-octo-gone.service",
      "toolu-runner.service",
    ] {
      stops.push(format!("--user disable --now {unit}"));
    }
  }
  for stop in &stops {
    let at = lines
      .iter()
      .position(|line| line == stop)
      .ok_or(format!("{stop:?} never ran:\n{calls}"))?;
    assert!(at < started, "{stop:?} must precede the start:\n{calls}");
  }
  assert!(
    !calls.contains("foreign"),
    "the foreign unit is never touched:\n{calls}"
  );
  for replaced in [&per_registration, &orphan] {
    assert!(
      stdout.contains(&format!("replaced {}", replaced.display())),
      "{stdout}"
    );
  }
  let removal = if MACOS {
    "launchctl bootout gui/$(id -u)/io.toolu.runner.foreign"
  } else {
    "systemctl --user disable --now toolu-runner-foreign.service"
  };
  assert!(
    stderr.contains(&format!("left {} running", foreign.display())) && stderr.contains(removal),
    "{stderr}"
  );
  Ok(())
}

/// `--no-activate` writes the machine unit only and names the old units it
/// would replace; `--remove` deletes only the machine unit, idempotently.
#[test]
fn no_activate_and_remove_touch_only_the_machine_unit() -> TestResult {
  let home = tempfile::tempdir()?;
  let demo = write_fixture(home.path(), "octo", "demo")?;
  let old = write_old_unit(
    home.path(),
    &unit_file(
      "toolu-runner-octo-demo.service",
      "io.toolu.runner.octo.demo.plist",
    ),
    "io.toolu.runner.octo.demo",
    &demo,
  )?;

  let (out, calls) = with_stub(home.path(), &["install-service", "--no-activate"])?;
  let stdout = text(&out.stdout);
  assert!(out.status.success(), "{stdout}{}", text(&out.stderr));
  assert_eq!(calls, "", "--no-activate activates nothing");
  assert_eq!(target(&machine_dest(home.path())), Some(Supervises::All));
  assert!(old.is_file(), "old units stay until a full install");
  assert!(
    stdout.contains(&format!("still installed: {}", old.display())),
    "{stdout}"
  );

  let (removed, _) = with_stub(home.path(), &["install-service", "--remove"])?;
  assert!(removed.status.success(), "{}", text(&removed.stderr));
  assert!(!machine_dest(home.path()).exists());
  assert!(old.is_file(), "--remove deletes only the machine unit");
  let (again, _) = with_stub(home.path(), &["install-service", "--remove"])?;
  assert!(
    text(&again.stdout).contains("nothing to do"),
    "{}",
    text(&again.stdout)
  );
  Ok(())
}

/// AC-13: the machine unit plus the registration's own unit is a double
/// supervision `doctor` fails; the machine unit alone is the one row.
#[test]
fn doctor_fails_a_registration_two_units_supervise() -> TestResult {
  let home = tempfile::tempdir()?;
  let demo = write_fixture(home.path(), "octo", "demo")?;
  let (out, _) = with_stub(home.path(), &["install-service", "--no-activate"])?;
  assert!(out.status.success(), "{}", text(&out.stderr));

  // Doctor runs with the stub too: it must never ask the host's real
  // supervisor about a unit in a temporary HOME.
  let (single, _) = with_stub(home.path(), &["doctor", "--offline"])?;
  let report = text(&single.stdout);
  let dest = machine_dest(home.path());
  assert!(
    report.lines().any(|l| l.contains(" service ")
      && l.contains("installed")
      && l.contains(&dest.display().to_string())),
    "{report}"
  );
  assert!(!report.contains("supervised by"), "{report}");

  let (made, _) = with_stub(
    home.path(),
    &[
      "install-service",
      "--no-activate",
      "--config",
      &demo.to_string_lossy(),
    ],
  )?;
  assert!(made.status.success(), "{}", text(&made.stderr));
  let (double, _) = with_stub(home.path(), &["doctor", "--offline"])?;
  let report = text(&double.stdout);
  assert_eq!(double.status.code(), Some(1), "{report}");
  let row = report
    .lines()
    .position(|l| {
      l.contains("FAIL") && l.contains("service") && l.contains("supervised by 2 units")
    })
    .ok_or(format!("no double-supervision row:\n{report}"))?;
  assert_eq!(
    report.lines().nth(row + 1).map(str::trim),
    Some("fix: toolu-runner install-service"),
    "{report}"
  );
  Ok(())
}

/// A legacy-root registration's pre-#208 unit was `toolu-runner.service`
/// (now the machine unit's name): `--config <home>/config.toml --remove`
/// still removes it.
#[cfg(not(target_os = "macos"))]
#[test]
fn removing_a_legacy_registration_unit_removes_its_pre_208_name() -> TestResult {
  let home = tempfile::tempdir()?;
  let demo = write_fixture(home.path(), "octo", "demo")?;
  let legacy = write_legacy(home.path(), &demo)?;
  let old = write_old_unit(
    home.path(),
    "toolu-runner.service",
    "io.toolu.runner",
    &legacy,
  )?;
  let config = legacy.to_string_lossy().into_owned();
  let (out, calls) = with_stub(
    home.path(),
    &["install-service", "--remove", "--config", &config],
  )?;
  assert!(out.status.success(), "{}", text(&out.stderr));
  assert!(!old.exists(), "{}", text(&out.stdout));
  assert!(
    calls.contains("--user disable --now toolu-runner.service"),
    "{calls}"
  );
  assert!(text(&out.stdout).contains(&format!("removed io.toolu.runner ({})", old.display())));
  Ok(())
}
