//! Exclusive advisory leases on work-root lock files (issue #90).
//!
//! The work root is shared by every registration, so each job holds an `fs2`
//! lock on a file under `_PipelineMapping/` while it uses its workspace, and
//! GC deletes only a workspace whose lease it can take itself. The lock
//! belongs to the open file (a second open in this process cannot take it);
//! dropping the lease releases it. Lease files are never unlinked while in use.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use fs2::FileExt;
use shared::RunnerError;

/// A held exclusive lock on one lease file; released when dropped.
#[derive(Debug)]
pub struct WorkspaceLease {
  file: File,
  path: PathBuf,
}

impl WorkspaceLease {
  /// Take the lease at `path` without waiting; `Ok(None)` when another open
  /// file holds it. Creates the file and its parent directories.
  ///
  /// # Errors
  ///
  /// [`RunnerError::WorkspaceInit`] when the file cannot be created or the
  /// lock call fails for a reason other than contention.
  pub fn try_acquire(path: &Path) -> Result<Option<Self>, RunnerError> {
    let file = open(path)?;
    match file.try_lock_exclusive() {
      Ok(()) => Ok(Some(Self {
        file,
        path: path.to_path_buf(),
      })),
      Err(error) if error.kind() == fs2::lock_contended_error().kind() => Ok(None),
      Err(source) => Err(init_error(path, source)),
    }
  }

  /// Take the lease at `path`, waiting for a current holder to release it.
  ///
  /// # Errors
  ///
  /// [`RunnerError::WorkspaceInit`] when the file cannot be created or locked.
  pub fn acquire(path: &Path) -> Result<Self, RunnerError> {
    let file = open(path)?;
    file
      .lock_exclusive()
      .map_err(|source| init_error(path, source))?;
    Ok(Self {
      file,
      path: path.to_path_buf(),
    })
  }

  /// Replace the lease file's body (e.g. the owning registration).
  ///
  /// # Errors
  ///
  /// [`RunnerError::WorkspaceInit`] when the file cannot be rewritten.
  pub fn write_body(&mut self, body: &[u8]) -> Result<(), RunnerError> {
    self
      .file
      .set_len(0)
      .and_then(|()| self.file.write_all(body))
      .and_then(|()| self.file.flush())
      .map_err(|source| init_error(&self.path, source))
  }
}

/// Open (creating) the lease file without truncating a holder's body.
fn open(path: &Path) -> Result<File, RunnerError> {
  if let Some(parent) = path.parent() {
    std::fs::create_dir_all(parent).map_err(|source| init_error(parent, source))?;
  }
  OpenOptions::new()
    .read(true)
    .write(true)
    .create(true)
    .truncate(false)
    .open(path)
    .map_err(|source| init_error(path, source))
}

fn init_error(path: &Path, source: std::io::Error) -> RunnerError {
  RunnerError::WorkspaceInit {
    path: path.to_path_buf(),
    source,
  }
}
