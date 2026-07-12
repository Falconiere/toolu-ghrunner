//! Azure "Put Blob" (single-shot): the whole object arrives in one PUT body,
//! the path the JS SDK takes for uploads at or below 128 MiB. The body is
//! written verbatim to the token's staging file.

use std::path::Path;

use axum::body::Bytes;
use shared::RunnerError;

/// Write `body` as the complete staged object at `staging`.
///
/// # Errors
/// `RunnerError::Io` if the parent directory or the file cannot be created or
/// written.
pub(super) async fn put(staging: &Path, body: Bytes) -> Result<(), RunnerError> {
  if let Some(parent) = staging.parent() {
    tokio::fs::create_dir_all(parent)
      .await
      .map_err(RunnerError::Io)?;
  }
  tokio::fs::write(staging, &body)
    .await
    .map_err(RunnerError::Io)?;
  Ok(())
}
