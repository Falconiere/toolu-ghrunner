//! Filesystem path helpers shared across crates.
//!
//! Currently houses `expand_tilde`, the only cross-crate path helper
//! needed in v1. `expand_tilde` resolves a leading `~` or `~/<rest>` to
//! the current user's home directory using the standard precedence
//! (HOME → USERPROFILE → `/var/lib/toolu-runner`). It does not touch
//! the filesystem, so it stays in the sync, I/O-free `shared` crate.

use std::path::{Path, PathBuf};

/// Resolve a leading `~` or `~/<rest>` against the user's home directory.
///
/// - `~` → home dir
/// - `~/foo` → home + `foo`
/// - `~user/foo` → returned unchanged (only the current user is expanded)
/// - any absolute or relative path not starting with `~` → returned unchanged
///
/// On non-Unix platforms the behavior is identical (we just use the HOME
/// env var; USERPROFILE support is incidental).
///
/// # Examples
///
/// ```
/// use shared::paths::expand_tilde;
/// let p = expand_tilde(std::path::Path::new("/etc/hosts"));
/// assert_eq!(p, std::path::PathBuf::from("/etc/hosts"));
/// ```
pub fn expand_tilde(path: &Path) -> PathBuf {
  let Some(raw) = path.to_str() else {
    return path.to_path_buf();
  };
  if raw == "~" {
    return home_dir();
  }
  if let Some(rest) = raw.strip_prefix("~/") {
    return home_dir().join(rest);
  }
  path.to_path_buf()
}

/// Sanitize a job id for use in a journal file name: every char outside
/// `[A-Za-z0-9._-]` becomes one `_`; no collapsing, no truncation.
pub fn sanitize_job_id(job_id: &str) -> String {
  job_id
    .chars()
    .map(|c| {
      if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
        c
      } else {
        '_'
      }
    })
    .collect()
}

/// Resolve the current user's home directory.
///
/// Tries HOME first, then USERPROFILE, then falls back to
/// `/var/lib/toolu-runner` (a system-level service install).
fn home_dir() -> PathBuf {
  if let Some(home) = std::env::var_os("HOME") {
    return PathBuf::from(home);
  }
  if let Some(profile) = std::env::var_os("USERPROFILE") {
    return PathBuf::from(profile);
  }
  PathBuf::from("/var/lib/toolu-runner")
}
