//! `doctor` service rows: whether a persistent registration has the
//! supervisor unit `install-service` writes, whether the supervisor has it
//! loaded (launchd) or active (systemd), and (systemd) whether user lingering
//! lets units start at boot. Whether `run` itself is up is the `run` row's
//! answer, from the lock body.
//!
//! Queries are read-only subprocesses (`launchctl print`, `systemctl --user
//! is-active`, `loginctl show-user`), each bounded by the probe timeout and
//! killed when it overruns. A query that cannot answer is a warning naming
//! the tool, never an error.

use std::path::Path;
use std::time::Duration;

use crate::check_tools::{RunError, first_line, run};
use crate::doctor_report::{Level, Row};
use crate::service_cmd::{
  Supervisor, activation_hint, current_supervisor, current_uid, dest_path, home_dir, service_id,
};

/// What the supervisor says about an installed unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Activity {
  /// Loaded by launchd (`launchctl print` answers) or active under
  /// systemd (`is-active`).
  Running,
  /// Installed but not running; carries the activation command.
  Stopped(String),
  /// The supervisor could not be asked: the tool and why.
  Unknown(String, String),
}

/// `systemctl --user is-active` output: running, stopped, or not an answer.
pub(crate) fn parse_is_active(stdout: &str) -> Option<bool> {
  match stdout.trim() {
    "active" | "reloading" | "activating" => Some(true),
    "inactive" | "failed" | "deactivating" => Some(false),
    _ => None,
  }
}

/// `loginctl show-user --property=Linger` output.
pub(crate) fn parse_linger(stdout: &str) -> Option<bool> {
  match stdout.trim() {
    "Linger=yes" => Some(true),
    "Linger=no" => Some(false),
    _ => None,
  }
}

/// The `service` row for a unit file that exists at `dest`.
pub(crate) fn installed_row(dest: &Path, activity: &Activity) -> Row {
  let dest = dest.display();
  match activity {
    Activity::Running => Row::new(
      Level::Ok,
      "service",
      format!("installed and active ({dest})"),
    ),
    Activity::Stopped(hint) => Row::new(
      Level::Warn,
      "service",
      format!("installed ({dest}), not active"),
    )
    .with_fix(hint.clone()),
    Activity::Unknown(tool, reason) => Row::new(
      Level::Warn,
      "service",
      format!("installed ({dest}); could not query {tool}: {reason}"),
    ),
  }
}

/// Why a query produced no answer, in one line.
fn run_error(error: RunError, timeout: Duration) -> String {
  match error {
    RunError::NotFound => "not found on PATH".to_owned(),
    RunError::TimedOut => format!("timed out after {} s", timeout.as_secs()),
    RunError::Spawn(error) => format!("could not start: {error}"),
  }
}

/// Ask the supervisor whether the unit is running.
async fn activity(
  supervisor: Supervisor,
  label: &str,
  unit: &str,
  hint: String,
  timeout: Duration,
) -> Activity {
  match supervisor {
    Supervisor::Launchd => {
      // `current_uid` runs `id -u`: keep the subprocess off the async workers.
      let uid = match tokio::task::spawn_blocking(current_uid).await {
        Ok(Ok(uid)) => uid,
        Ok(Err(error)) => return Activity::Unknown("launchctl".to_owned(), error.to_string()),
        Err(error) => return Activity::Unknown("launchctl".to_owned(), error.to_string()),
      };
      let target = format!("gui/{uid}/{label}");
      match run("launchctl", &["print", &target], timeout).await {
        Ok(ran) if ran.success => Activity::Running,
        Ok(_) => Activity::Stopped(hint),
        Err(error) => Activity::Unknown("launchctl".to_owned(), run_error(error, timeout)),
      }
    },
    Supervisor::Systemd => match run("systemctl", &["--user", "is-active", unit], timeout).await {
      Ok(ran) => match parse_is_active(&ran.stdout) {
        Some(true) => Activity::Running,
        Some(false) => Activity::Stopped(hint),
        None => Activity::Unknown("systemctl".to_owned(), first_line(&ran.stderr).to_owned()),
      },
      Err(error) => Activity::Unknown("systemctl".to_owned(), run_error(error, timeout)),
    },
  }
}

/// The `service` row for the registration at `config_path`, plus whether a
/// unit file exists. `None` for an ephemeral registration (never supervised).
pub(crate) async fn service_row(
  config_path: &Path,
  ephemeral: bool,
  timeout: Duration,
) -> Option<(Row, bool)> {
  if ephemeral {
    return None;
  }
  let Ok(supervisor) = current_supervisor() else {
    return Some((
      Row::new(
        Level::Skip,
        "service",
        "supervisor units are launchd (macOS) / systemd (Linux) only",
      ),
      false,
    ));
  };
  let home = match home_dir() {
    Ok(home) => home,
    Err(error) => return Some((Row::new(Level::Warn, "service", error.to_string()), false)),
  };
  let id = service_id(config_path);
  let dest = dest_path(supervisor, &id, &home);
  if !dest.exists() {
    let row = Row::new(Level::Warn, "service", "not installed").with_fix(format!(
      "toolu-runner install-service --config {}",
      config_path.display()
    ));
    return Some((row, false));
  }
  let hint = activation_hint(supervisor, &dest, &id);
  let activity = activity(supervisor, &id.label, &id.unit, hint, timeout).await;
  Some((installed_row(&dest, &activity), true))
}

/// The `linger` row for `user` given `loginctl`'s parsed answer, or why
/// there was none.
pub(crate) fn linger_row_for(user: &str, linger: Result<bool, String>) -> Row {
  match linger {
    Ok(true) => Row::new(
      Level::Ok,
      "linger",
      format!("enabled for {user}: user units start at boot"),
    ),
    Ok(false) => Row::new(
      Level::Warn,
      "linger",
      format!("disabled for {user}: user units start only at login"),
    )
    .with_fix(format!("loginctl enable-linger {user}")),
    Err(reason) => Row::new(
      Level::Warn,
      "linger",
      format!("could not query loginctl for {user}: {reason}"),
    ),
  }
}

/// The `linger` row on systemd hosts with at least one installed unit.
pub(crate) async fn linger_row(any_unit_installed: bool, timeout: Duration) -> Option<Row> {
  if !any_unit_installed || !matches!(current_supervisor(), Ok(Supervisor::Systemd)) {
    return None;
  }
  let user = match run("id", &["-un"], timeout).await {
    Ok(ran) if ran.success && !ran.stdout.is_empty() => ran.stdout,
    Ok(ran) => {
      let said = first_line(&ran.stderr);
      let reason = if said.is_empty() {
        "`id -un` gave no user name".to_owned()
      } else {
        format!("`id -un` failed: {said}")
      };
      return Some(linger_row_for("this user", Err(reason)));
    },
    Err(error) => {
      let reason = format!("`id -un`: {}", run_error(error, timeout));
      return Some(linger_row_for("this user", Err(reason)));
    },
  };
  let linger = match run(
    "loginctl",
    &["show-user", &user, "--property=Linger"],
    timeout,
  )
  .await
  {
    Ok(ran) => parse_linger(&ran.stdout).ok_or_else(|| {
      let said = first_line(&ran.stderr);
      if said.is_empty() {
        format!("unexpected output {:?}", ran.stdout.trim())
      } else {
        said.to_owned()
      }
    }),
    Err(error) => Err(run_error(error, timeout)),
  };
  Some(linger_row_for(&user, linger))
}

#[cfg(test)]
#[path = "tests/doctor_service.rs"]
mod tests;
