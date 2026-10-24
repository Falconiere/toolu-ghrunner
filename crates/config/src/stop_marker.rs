//! Why a supervised registration stopped (#208).
//!
//! `toolu-runner run --all` drives every registration in one process. When
//! one registration's loop ends with a fatal error (a rejected re-mint, a
//! session conflict, a corrupt config) only that loop stops; the supervisor
//! records the reason in `<data_dir>/.stopped` so `status` and `doctor` can
//! name it after the fact. The loop clears the marker when it starts again,
//! and `remove` deletes it with the rest of the registration.
//!
//! The body is JSON, written owner-only (0600) like every other runner state
//! file. The error text is masked by the writer before it gets here.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use shared::RunnerError;

/// The marker's file name inside a registration's data dir.
pub const FILE_NAME: &str = ".stopped";

/// JSON body of `<data_dir>/.stopped`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopMarker {
  /// When the loop stopped (RFC 3339).
  pub stopped_at: String,
  /// The fatal error that stopped it, already secret-masked.
  pub error: String,
}

/// `<data_dir>/.stopped`.
#[must_use]
pub fn path(data_dir: &Path) -> PathBuf {
  data_dir.join(FILE_NAME)
}

/// Write `marker` to `<data_dir>/.stopped` (0600), creating `data_dir`.
///
/// # Errors
///
/// `RunnerError::Io` when the dir or file cannot be written.
pub fn write(data_dir: &Path, marker: &StopMarker) -> Result<(), RunnerError> {
  std::fs::create_dir_all(data_dir)?;
  let body = serde_json::to_string_pretty(marker)
    .map_err(|e| RunnerError::Config(format!("json encode: {e}")))?;
  crate::config::write_secret_file(&path(data_dir), body.as_bytes())
}

/// Read `<data_dir>/.stopped`; `Ok(None)` when there is none.
///
/// # Errors
///
/// `RunnerError::Config` naming the file when it exists but cannot be read
/// or parsed.
pub fn read(data_dir: &Path) -> Result<Option<StopMarker>, RunnerError> {
  let file = path(data_dir);
  let raw = match std::fs::read_to_string(&file) {
    Ok(raw) => raw,
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
    Err(e) => {
      return Err(RunnerError::Config(format!(
        "cannot read {}: {e}",
        file.display()
      )));
    },
  };
  serde_json::from_str(&raw)
    .map(Some)
    .map_err(|e| RunnerError::Config(format!("cannot parse {}: {e}", file.display())))
}

/// Delete `<data_dir>/.stopped`; a missing marker is not an error.
///
/// # Errors
///
/// `RunnerError::Io` when the file exists but cannot be removed.
pub fn clear(data_dir: &Path) -> Result<(), RunnerError> {
  match std::fs::remove_file(path(data_dir)) {
    Ok(()) => Ok(()),
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
    Err(e) => Err(e.into()),
  }
}
