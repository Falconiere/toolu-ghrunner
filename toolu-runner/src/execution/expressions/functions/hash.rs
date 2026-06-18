//! SHA-256 file hashing for the `hashFiles()` expression function.

use shared::RunnerError;

/// Compute SHA-256 hash of files matching glob patterns in a workspace.
///
/// Files are sorted lexicographically for deterministic output.
/// Returns empty string if no files match.
///
/// # Errors
///
/// Returns `RunnerError::Expression` on glob or IO failures.
pub fn hash_files(workspace: &std::path::Path, patterns: &[&str]) -> Result<String, RunnerError> {
  use sha2::{Digest, Sha256};

  let mut matched_files = Vec::new();

  for pattern in patterns {
    let full = workspace.join(pattern).to_string_lossy().to_string();
    let paths =
      glob::glob(&full).map_err(|e| RunnerError::Expression(format!("hashFiles glob: {e}")))?;

    for entry in paths {
      let path = entry.map_err(|e| RunnerError::Expression(format!("hashFiles entry: {e}")))?;
      if path.is_file() {
        matched_files.push(path);
      }
    }
  }

  if matched_files.is_empty() {
    return Ok(String::new());
  }

  matched_files.sort();
  matched_files.dedup();

  let mut hasher = Sha256::new();
  for file in &matched_files {
    let content =
      std::fs::read(file).map_err(|e| RunnerError::Expression(format!("hashFiles read: {e}")))?;
    hasher.update(&content);
  }

  Ok(format!("{:x}", hasher.finalize()))
}
