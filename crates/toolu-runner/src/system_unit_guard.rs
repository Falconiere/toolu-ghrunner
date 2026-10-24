//! Collision checks for root per-registration systemd unit files.

use std::path::Path;

use config::service_unit::{Supervises, parse_systemd_target};
use shared::RunnerError;

/// Reuse only this registration's unit; reject symlinks and live foreign units.
pub(super) fn check_existing_system_unit(
  dest: &Path,
  config_path: &Path,
) -> Result<(), RunnerError> {
  let metadata = match std::fs::symlink_metadata(dest) {
    Ok(metadata) => metadata,
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
    Err(error) => {
      return Err(RunnerError::Config(format!(
        "cannot inspect system unit {}: {error}",
        dest.display()
      )));
    },
  };
  if metadata.file_type().is_symlink() {
    return Err(RunnerError::Config(format!(
      "{} is a symlink; remove it before installing so its target is not overwritten",
      dest.display()
    )));
  }
  let text = std::fs::read_to_string(dest).map_err(|error| {
    RunnerError::Config(format!(
      "cannot read system unit {}: {error}",
      dest.display()
    ))
  })?;
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
    _ => false,
  }
}
