//! Tests for the `doctor` service rows: parsing the real one-line answers of
//! `systemctl --user is-active` and `loginctl show-user --property=Linger`,
//! the row each unit state renders, and the supervisor query against the
//! real tool (or its absence) on this host.

use std::path::Path;
use std::time::Duration;

use super::{
  Activity, activity, installed_row, linger_row, linger_row_for, parse_is_active, parse_linger,
  run_error, service_row,
};
use crate::check_tools::RunError;
use crate::doctor_report::{Level, Row};
use crate::service_cmd::{Supervisor, current_supervisor};

const UNIT: &str = "/h/.config/systemd/user/toolu-runner-o-r.service";

#[test]
fn systemctl_is_active_states_map_to_running_or_not() {
  for running in ["active\n", "reloading\n", "activating\n"] {
    assert_eq!(parse_is_active(running), Some(true), "{running:?}");
  }
  for stopped in ["inactive\n", "failed\n", "deactivating\n"] {
    assert_eq!(parse_is_active(stopped), Some(false), "{stopped:?}");
  }
  assert_eq!(parse_is_active(""), None);
  assert_eq!(parse_is_active("Failed to connect to bus\n"), None);
  // Only the whole answer counts, not a word inside another message.
  assert_eq!(parse_is_active("not active\n"), None);
}

#[test]
fn loginctl_linger_property_is_parsed() {
  assert_eq!(parse_linger("Linger=yes\n"), Some(true));
  assert_eq!(parse_linger("Linger=no\n"), Some(false));
  assert_eq!(parse_linger(""), None);
  assert_eq!(parse_linger("Linger=maybe\n"), None);
  assert_eq!(parse_linger("Failed to get user: No such user\n"), None);
}

#[test]
fn unit_states_render_ok_warn_with_activation_or_query_failure() {
  let dest = Path::new(UNIT);
  assert_eq!(
    installed_row(dest, &Activity::Running),
    Row::new(
      Level::Ok,
      "service",
      "installed and active (/h/.config/systemd/user/toolu-runner-o-r.service)"
    )
  );

  assert_eq!(
    installed_row(
      dest,
      &Activity::Stopped("systemctl --user enable --now toolu-runner-o-r.service".to_owned()),
    ),
    Row::new(
      Level::Warn,
      "service",
      "installed (/h/.config/systemd/user/toolu-runner-o-r.service), not active"
    )
    .with_fix("systemctl --user enable --now toolu-runner-o-r.service")
  );

  assert_eq!(
    installed_row(
      dest,
      &Activity::Unknown(
        "systemctl".to_owned(),
        "Failed to connect to bus".to_owned(),
      ),
    ),
    Row::new(
      Level::Warn,
      "service",
      "installed (/h/.config/systemd/user/toolu-runner-o-r.service); could not query systemctl: Failed to connect to bus"
    )
  );
}

#[test]
fn linger_off_warns_with_enable_linger_for_the_user() {
  assert_eq!(
    linger_row_for("ci", Ok(true)),
    Row::new(
      Level::Ok,
      "linger",
      "enabled for ci: user units start at boot"
    )
  );
  assert_eq!(
    linger_row_for("ci", Ok(false)),
    Row::new(
      Level::Warn,
      "linger",
      "disabled for ci: user units start only at login"
    )
    .with_fix("loginctl enable-linger ci")
  );
  assert_eq!(
    linger_row_for("ci", Err("not found on PATH".to_owned())),
    Row::new(
      Level::Warn,
      "linger",
      "could not query loginctl for ci: not found on PATH"
    )
  );
}

#[test]
fn a_query_that_cannot_run_is_explained_in_one_line() {
  assert_eq!(
    run_error(RunError::NotFound, Duration::from_secs(3)),
    "not found on PATH"
  );
  assert_eq!(
    run_error(RunError::TimedOut, Duration::from_secs(3)),
    "timed out after 3 s"
  );
  assert_eq!(
    run_error(
      RunError::Spawn("Permission denied (os error 13)".to_owned()),
      Duration::from_secs(3)
    ),
    "could not start: Permission denied (os error 13)"
  );
}

#[tokio::test]
async fn an_ephemeral_registration_has_no_service_row() {
  assert!(
    service_row(
      Path::new("/h/runners/o/r/config.toml"),
      true,
      Duration::from_secs(1)
    )
    .await
    .is_none()
  );
}

#[tokio::test]
async fn linger_is_only_asked_about_for_installed_units_on_systemd() {
  assert_eq!(linger_row(false, Duration::from_secs(1)).await, None);
  if !matches!(current_supervisor(), Ok(Supervisor::Systemd)) {
    assert_eq!(linger_row(true, Duration::from_secs(1)).await, None);
  }
}

#[tokio::test]
async fn the_foreign_supervisors_tool_is_reported_as_not_found_on_path() {
  let (foreign, tool) = if matches!(current_supervisor(), Ok(Supervisor::Launchd)) {
    (Supervisor::Systemd, "systemctl")
  } else {
    (Supervisor::Launchd, "launchctl")
  };
  let got = activity(
    foreign,
    "io.toolu.runner.doctor-test.absent",
    "toolu-runner-doctor-test-absent.service",
    "hint".to_owned(),
    Duration::from_secs(5),
  )
  .await;
  assert_eq!(
    got,
    Activity::Unknown(tool.to_owned(), "not found on PATH".to_owned())
  );
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn launchd_reports_an_unloaded_label_as_stopped_with_the_activation_hint() {
  let hint = "launchctl bootstrap gui/$(id -u) /h/Library/LaunchAgents/x.plist".to_owned();
  let got = activity(
    Supervisor::Launchd,
    "io.toolu.runner.doctor-test.absent",
    "unused.service",
    hint.clone(),
    Duration::from_secs(5),
  )
  .await;
  assert_eq!(got, Activity::Stopped(hint));
}

/// #208: a registration only the static `install.sh` system unit runs gets a
/// row about that system unit, never a `systemctl --user` query of the
/// user's machine unit name.
#[test]
fn a_system_scope_unit_gets_a_system_row() -> Result<(), Box<dyn std::error::Error>> {
  let dir = tempfile::tempdir()?;
  let system = dir.path().join("system");
  std::fs::create_dir_all(&system)?;
  let unit_path = system.join("toolu-runner.service");
  std::fs::write(
    &unit_path,
    include_str!("../../../../scripts/toolu-runner.service"),
  )?;
  let units = crate::machine_service::installed_units(
    Supervisor::Systemd,
    &dir.path().join("user"),
    &dir.path().join("runner-home"),
    Some(&system),
  );
  let unit = units.first().ok_or("the system unit is not inventoried")?;
  assert!(unit.supervises_registration(Path::new("/etc/toolu-runner/config.toml")));
  assert_eq!(
    super::system_unit_row(unit),
    Row::new(
      Level::Warn,
      "service",
      format!(
        "system unit {} (system scope; #203 manages system units)",
        unit_path.display()
      ),
    )
    .with_fix("systemctl status toolu-runner.service")
  );
  Ok(())
}
