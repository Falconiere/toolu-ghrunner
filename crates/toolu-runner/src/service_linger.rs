//! Linger preflight for non-root systemd service installation.
//!
//! Linger is checked before migration or unit writes. A failed authorization
//! leaves the existing units intact. Root system units and launchd skip it.

use std::io::{BufRead, Write};

use shared::RunnerError;

use crate::cli::InstallServiceArgs;
use crate::doctor_service::parse_linger;
use crate::service_cmd::{Supervisor, UnitScope, run_checked, run_stdout};

/// The user's choice after the preflight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LingerDecision {
  /// Already enabled, just enabled, or irrelevant to this supervisor.
  Persistent,
  /// The user chose to install a unit that stops after logout.
  StopOnLogout,
}

/// Ensure a user unit can persist, or record a deliberate short-lived choice.
/// Call only for an install, after config and destination checks but before
/// replacing or writing any unit.
pub(crate) fn before_install(
  supervisor: Supervisor,
  scope: UnitScope,
  args: &InstallServiceArgs,
) -> Result<LingerDecision, RunnerError> {
  if supervisor != Supervisor::Systemd || scope == UnitScope::System {
    return Ok(LingerDecision::Persistent);
  }
  let user = run_stdout("id", &["-un"])?;
  if user.is_empty() {
    return Err(RunnerError::Config(
      "`id -un` returned no user name".to_owned(),
    ));
  }
  if query_linger(&user)? {
    return Ok(LingerDecision::Persistent);
  }
  if args.linger.enable_linger || args.linger.yes || ask(&user)? {
    enable(&user)?;
    return Ok(LingerDecision::Persistent);
  }
  Ok(LingerDecision::StopOnLogout)
}

/// Warn only after the unit was installed successfully.
pub(crate) fn warn_if_short_lived(decision: LingerDecision) {
  if decision == LingerDecision::StopOnLogout {
    eprintln!("warning: linger is disabled; this user service stops when you log out");
  }
}

fn query_linger(user: &str) -> Result<bool, RunnerError> {
  let output =
    run_stdout("loginctl", &["show-user", user, "--property=Linger"]).map_err(|error| {
      RunnerError::Config(format!(
        "Couldn't query linger for {user}: {error}. Nothing was installed."
      ))
    })?;
  parse_linger(&output).ok_or_else(|| RunnerError::Config(format!(
    "Couldn't read linger for {user}: unexpected `loginctl` output {output:?}. Nothing was installed."
  )))
}

fn enable(user: &str) -> Result<(), RunnerError> {
  run_checked("loginctl", &["enable-linger", user]).map_err(|error| {
    let said = error.to_string();
    let lower = said.to_ascii_lowercase();
    let reason = if lower.contains("polkit") || lower.contains("access denied") {
      "polkit denied it".to_owned()
    } else {
      said
    };
    RunnerError::Config(format!(
      "Couldn't enable linger: {reason}. Nothing was installed.\nRun: sudo loginctl enable-linger {user}\nThen: toolu-runner setup"
    ))
  })?;
  if !query_linger(user)? {
    return Err(RunnerError::Config(format!(
      "Couldn't confirm linger for {user} after enabling it. Nothing was installed.\nRun: sudo loginctl enable-linger {user}\nThen: toolu-runner setup"
    )));
  }
  Ok(())
}

fn ask(user: &str) -> Result<bool, RunnerError> {
  let mut tty = std::fs::OpenOptions::new()
    .read(true)
    .write(true)
    .open("/dev/tty")
    .map_err(|error| {
      RunnerError::Config(format!(
        "linger is disabled and no terminal is available ({error}); pass --enable-linger or --yes"
      ))
    })?;
  let mut reader = std::io::BufReader::new(tty.try_clone().map_err(|error| {
    RunnerError::Config(format!(
      "cannot open a reader for the linger terminal: {error}"
    ))
  })?);
  let mut input = String::new();
  loop {
    tty.write_all(format!("Keep running after you log out?\nRuns: loginctl enable-linger {user}\n[y] Yes / [n] No, stop when I log out: ").as_bytes()).map_err(|error| {
      RunnerError::Config(format!("cannot write the linger prompt to the terminal: {error}"))
    })?;
    tty
      .flush()
      .map_err(|error| RunnerError::Config(format!("cannot flush the linger prompt: {error}")))?;
    input.clear();
    if reader.read_line(&mut input).map_err(|error| {
      RunnerError::Config(format!(
        "cannot read a linger answer from the terminal: {error}"
      ))
    })?
      == 0
    {
      return Err(RunnerError::Config(
        "no linger answer was given; nothing was installed".to_owned(),
      ));
    }
    match input.trim().to_ascii_lowercase().as_str() {
      "y" | "yes" => return Ok(true),
      "n" | "no" => return Ok(false),
      _ => tty.write_all(b"Please answer y or n.\n").map_err(|error| {
        RunnerError::Config(format!("cannot write the linger retry prompt: {error}"))
      })?,
    }
  }
}
