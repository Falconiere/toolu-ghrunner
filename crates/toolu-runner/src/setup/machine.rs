//! Machine check (screen 02): asks nothing. Reports OS/arch/CPU and
//! memory, the service manager and linger state, whether the session
//! looks headless, the runner home and the existing registrations —
//! and derives the defaults those findings set for later steps.

use std::path::PathBuf;

use config::registry::{self, RegistrationEntry};

/// What the machine check found.
#[derive(Debug)]
pub(crate) struct MachineFacts {
  /// OS and architecture in GitHub spelling (`Linux X64`).
  pub(crate) platform: String,
  /// Logical CPU count.
  pub(crate) cpus: usize,
  /// Total memory in MiB, when it can be read.
  pub(crate) memory_mb: Option<u64>,
  /// Service manager: `systemd`, `launchd`, or `none detected`.
  pub(crate) supervisor: String,
  /// Linger state on systemd user installs, when known.
  pub(crate) linger: Option<bool>,
  /// SSH session or no display: App creation needs a steer.
  pub(crate) headless: bool,
  /// The runner home registrations live under.
  pub(crate) home: PathBuf,
  /// Registrations already on this machine.
  pub(crate) existing: Vec<RegistrationEntry>,
}

/// Gather the machine facts for `home`.
///
/// # Errors
///
/// A string when the registration scan fails (an unreadable registry
/// must not be mistaken for an empty machine).
pub(crate) fn gather(home: &std::path::Path) -> Result<MachineFacts, String> {
  let existing = registry::list_registrations(home).map_err(|e| e.to_string())?;
  Ok(MachineFacts {
    platform: format!(
      "{} {}",
      shared::platform::runner_os(),
      shared::platform::runner_arch()
    ),
    cpus: std::thread::available_parallelism().map_or(1, std::num::NonZero::get),
    memory_mb: total_memory_mb(),
    supervisor: supervisor_name(),
    linger: linger_state(),
    headless: classify_headless(
      crate::config::ssh_session(),
      crate::config::display_present(),
      cfg!(target_os = "macos"),
    ),
    home: home.to_path_buf(),
    existing,
  })
}

/// Whether the session looks headless: SSH always is; on macOS a
/// local GUI session sets neither `DISPLAY` nor `WAYLAND_DISPLAY`, so
/// only SSH counts there.
pub(crate) fn classify_headless(ssh: bool, display: bool, macos: bool) -> bool {
  ssh || (!macos && !display)
}

/// Total memory in MiB (Linux and macOS alike); `None` when the platform
/// reports none, rather than a guessed number.
fn total_memory_mb() -> Option<u64> {
  let mut system = sysinfo::System::new();
  system.refresh_memory();
  Some(system.total_memory() / 1_048_576).filter(|mb| *mb > 0)
}

/// The service manager name for display.
fn supervisor_name() -> String {
  match crate::service_cmd::current_supervisor() {
    Ok(crate::service_cmd::Supervisor::Systemd) => "systemd".to_owned(),
    Ok(crate::service_cmd::Supervisor::Launchd) => "launchd".to_owned(),
    Err(_) => "none detected".to_owned(),
  }
}

/// Linger for the current user on systemd, when it can be queried
/// without prompting.
fn linger_state() -> Option<bool> {
  if !cfg!(target_os = "linux") {
    return None;
  }
  let user = crate::service_cmd::run_stdout("id", &["-un"]).ok()?;
  let out =
    crate::service_cmd::run_stdout("loginctl", &["show-user", &user, "--property=Linger"]).ok()?;
  crate::doctor_service::parse_linger(&out)
}

impl MachineFacts {
  /// The `setup: machine …` summary for the plain renderer.
  pub(crate) fn summary(&self) -> String {
    let memory = self.memory_mb.map_or_else(
      || "memory unknown".to_owned(),
      |mb| format!("{} GiB", mb / 1024),
    );
    let linger = match self.linger {
      Some(true) => ", linger on",
      Some(false) => ", linger off",
      None => "",
    };
    let session = if self.headless { "headless" } else { "desktop" };
    format!(
      "{} · {} CPUs · {memory} · {}{} · {session} · home {} · {} existing runner(s)",
      self.platform,
      self.cpus,
      self.supervisor,
      linger,
      self.home.display(),
      self.existing.len()
    )
  }
}

#[cfg(test)]
#[path = "tests/machine.rs"]
mod tests;
