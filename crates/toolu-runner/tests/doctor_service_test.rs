//! Shell-out tests for the `service` row of `toolu-runner doctor`: a
//! registration without an installed unit warns with the install command, an
//! ephemeral one has no service row, and the unit the real `install-service
//! --no-activate` writes is reported as installed but not active.

use crate::check_servers::run;
use crate::doctor_env::{
  block, doctor_cmd, fix, has_lines, install_fix, report, row, write_registration,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn service_unit() -> TestResult {
  let home = tempfile::tempdir()?;
  let config = write_registration(home.path(), "o", "r", "http://127.0.0.1:9", false)?;
  write_registration(home.path(), "o", "once", "http://127.0.0.1:9", true)?;

  // Without a unit: an exact WARN with the install command.
  let (_, stdout, _, _) = run(doctor_cmd(home.path())?.arg("--offline"))?;
  assert!(
    has_lines(
      &block(&stdout, "o/r"),
      &[
        row("WARN", "service", "not installed"),
        install_fix(&config)
      ]
    ),
    "{stdout}"
  );
  // An ephemeral registration is never supervised: no service row.
  assert!(
    !block(&stdout, "o/once")
      .iter()
      .any(|line| line.starts_with("  OK    service ") || line.contains("  service    ")),
    "{stdout}"
  );

  // The unit the real `install-service --no-activate` writes into this HOME.
  let (code, out, err, _) = run(
    std::process::Command::new(env!("CARGO_BIN_EXE_toolu-runner"))
      .args(["install-service", "--no-activate", "--config"])
      .arg(&config)
      .current_dir(home.path())
      .env("HOME", home.path())
      .env("TOOLU_RUNNER_HOME", home.path()),
  )?;
  assert_eq!(code, Some(0), "{}", report(&out, &err));
  let macos = cfg!(target_os = "macos");
  let dest = if macos {
    home
      .path()
      .join("Library/LaunchAgents/io.toolu.runner.o.r.plist")
  } else {
    home
      .path()
      .join(".config/systemd/user/toolu-runner-o-r.service")
  };
  assert!(dest.is_file(), "no unit at {}", dest.display());
  let hint = if macos {
    format!("launchctl bootstrap gui/$(id -u) {}", dest.display())
  } else {
    "systemctl --user daemon-reload && systemctl --user enable --now toolu-runner-o-r.service"
      .to_owned()
  };

  let (_, stdout, _, _) = run(doctor_cmd(home.path())?.arg("--offline"))?;
  let rows = block(&stdout, "o/r");
  let stopped = [
    row(
      "WARN",
      "service",
      &format!("installed ({}), not active", dest.display()),
    ),
    fix(&hint),
  ];
  // launchd answers a non-zero `print` for a label that is not loaded. A
  // systemd user bus may be absent (a container), which `doctor` reports as
  // an unqueryable supervisor rather than a stopped one.
  let unqueryable = row(
    "WARN",
    "service",
    &format!(
      "installed ({}); could not query systemctl: ",
      dest.display()
    ),
  );
  assert!(
    has_lines(&rows, &stopped)
      || (!macos && rows.iter().any(|line| line.starts_with(&unqueryable))),
    "{stdout}"
  );
  Ok(())
}
