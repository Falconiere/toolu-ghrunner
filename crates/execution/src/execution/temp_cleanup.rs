//! Empties the runner's temp directories around every job, as the official
//! runner's `TempDirectoryManager` does: contents only, past delete errors.
//! Nothing else under `data_dir` is touched: `_tool` is kept for later jobs, and
//! the listener owns the `_logs` spools. A failure is a `WARN` and never changes
//! the job's result.
//!
//! `std::fs::remove_dir_all` is not used because it stops at the first error.

use std::io;
use std::path::{Path, PathBuf};

use shared::RunnerConfig;

use super::context::{restrict_dir_permissions, runner_temp_dir};

/// What one root's walk could not delete.
#[derive(Default)]
struct Failures {
  count: usize,
  first: Option<(PathBuf, io::Error)>,
}

impl Failures {
  /// Count a failed delete. An entry that already vanished is not a failure.
  fn check(&mut self, path: &Path, result: io::Result<()>) {
    if let Err(error) = result
      && error.kind() != io::ErrorKind::NotFound
    {
      self.count = self.count.saturating_add(1);
      self
        .first
        .get_or_insert_with(|| (path.to_path_buf(), error));
    }
  }
}

/// Empty `_temp` and `tmp`, warning once per root that keeps anything.
///
/// Warns on the calling task, not the blocking thread, so the job's subscriber
/// sees it.
pub(super) async fn empty_temp_dirs(config: &RunnerConfig) {
  let data_dir = &config.data_dir;
  for root in [runner_temp_dir(data_dir), data_dir.join("tmp")] {
    let walked = root.clone();
    match tokio::task::spawn_blocking(move || empty_root(&walked)).await {
      Ok(Failures {
        count,
        first: Some((path, error)),
      }) => tracing::warn!(
        root = %root.display(),
        failed = count,
        path = %path.display(),
        %error,
        "could not empty runner temp directory; continuing"
      ),
      Ok(_) => {},
      Err(error) => tracing::warn!(
        root = %root.display(),
        %error,
        "runner temp cleanup task did not complete; continuing"
      ),
    }
  }
}

/// Empty one root. A step may have replaced it, so only a real directory is
/// entered; a link or file in its place is removed, never its target.
fn empty_root(root: &Path) -> Failures {
  let mut failures = Failures::default();
  match std::fs::symlink_metadata(root) {
    Ok(metadata) if metadata.is_dir() => empty_dir(root, &mut failures),
    Ok(_) => failures.check(root, std::fs::remove_file(root)),
    Err(error) => failures.check(root, Err(error)),
  }
  failures
}

/// Delete everything in `dir`, continuing past failures. Entries are listed
/// first because deleting while reading a directory can skip entries on macOS.
fn empty_dir(dir: &Path, failures: &mut Failures) {
  let entries = std::fs::read_dir(dir).and_then(Iterator::collect::<io::Result<Vec<_>>>);
  let entries = match entries {
    Ok(entries) => entries,
    Err(error) => return failures.check(dir, Err(error)),
  };
  for entry in entries {
    let path = entry.path();
    // `file_type` does not follow symlinks, so a link is unlinked, never entered.
    match entry.file_type() {
      Ok(kind) if kind.is_dir() => remove_tree(&path, failures),
      Ok(_) => failures.check(&path, std::fs::remove_file(&path)),
      Err(error) => failures.check(&path, Err(error)),
    }
  }
}

/// Delete a directory and its contents, first making it writable: tools leave
/// read-only trees (Go's module cache) and upstream clears that flag as well.
fn remove_tree(dir: &Path, failures: &mut Failures) {
  // Refused for a directory owned by someone else; the delete below reports it.
  if let Err(error) = restrict_dir_permissions(dir) {
    tracing::debug!(path = %dir.display(), %error, "could not make temp directory writable");
  }
  let before = failures.count;
  empty_dir(dir, failures);
  if failures.count == before {
    failures.check(dir, std::fs::remove_dir(dir));
  }
}
