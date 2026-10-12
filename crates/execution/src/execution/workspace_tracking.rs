//! `_PipelineMapping/<owner>/<repo>/PipelineFolder.json` tracking files.
//!
//! Upstream `TrackingConfig` keys as the official runner writes them
//! (camelCase: `repositoryName`, `pipelineDirectory`, `workspaceDirectory`,
//! `repositories.*.repositoryPath`; observed in workspace-90 run 38177256043),
//! plus the toolu fields GC needs: `lastRunOnUnix` (upstream writes a
//! locale-formatted `lastRunOn`) and `dataDir`, the registration that last
//! used the workspace.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use shared::RunnerError;

/// Work-root directory holding tracking files and leases (upstream name).
pub const MAPPING_DIR: &str = "_PipelineMapping";
/// Tracking file name inside `_PipelineMapping/<owner>/<repo>/`.
pub const TRACKING_FILE: &str = "PipelineFolder.json";
/// Lease file name beside a tracking file, and the work-root lock's name.
pub const LEASE_FILE: &str = ".lock";
/// `_PipelineMapping/` child holding fresh per-job leases. GitHub owner names
/// cannot start with `_`, so it never collides with an owner directory.
pub const JOB_LEASES_DIR: &str = "_jobs";

/// One repository's stable workspace, as recorded on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackingConfig {
  /// `owner/repo`.
  pub repository_name: String,
  /// Work-root child holding the workspace (`runner.workspace`).
  pub pipeline_directory: String,
  /// Work-root-relative workspace (`github.workspace`): `<pipeline>/<repo>`.
  pub workspace_directory: String,
  /// Tracked repository paths, keyed by `owner/repo`.
  #[serde(default)]
  pub repositories: BTreeMap<String, RepositoryTracking>,
  /// Unix seconds of the last job that used this workspace.
  #[serde(default)]
  pub last_run_on_unix: u64,
  /// `data_dir` of the registration that last used this workspace.
  #[serde(default)]
  pub data_dir: PathBuf,
}

/// One tracked repository checkout inside the pipeline directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositoryTracking {
  /// Work-root-relative checkout path.
  pub repository_path: String,
}

impl TrackingConfig {
  /// A new mapping of `full_name` (`owner/name`) to `<pipeline>/<name>`.
  pub fn new(full_name: &str, name: &str, pipeline: &str) -> Self {
    let workspace = format!("{pipeline}/{name}");
    Self {
      repository_name: full_name.to_owned(),
      pipeline_directory: pipeline.to_owned(),
      workspace_directory: workspace.clone(),
      repositories: BTreeMap::from([(
        full_name.to_owned(),
        RepositoryTracking {
          repository_path: workspace,
        },
      )]),
      last_run_on_unix: 0,
      data_dir: PathBuf::new(),
    }
  }

  /// Whether this mapping is for `full_name` and only names directories
  /// directly under the work root (never absolute, `..`, or nested).
  pub fn is_usable_for(&self, full_name: &str) -> bool {
    self.repository_name.eq_ignore_ascii_case(full_name)
      && is_single_component(&self.pipeline_directory)
      && !self.pipeline_directory.eq_ignore_ascii_case(MAPPING_DIR)
      && Path::new(&self.workspace_directory)
        .strip_prefix(&self.pipeline_directory)
        .is_ok_and(|rest| is_single_component(&rest.to_string_lossy()))
  }

  /// Read a tracking file; `Ok(None)` when absent.
  ///
  /// # Errors
  ///
  /// [`RunnerError::Io`] when it cannot be read, [`RunnerError::Json`]
  /// when it is not a tracking document.
  pub fn load(path: &Path) -> Result<Option<Self>, RunnerError> {
    match std::fs::read_to_string(path) {
      Ok(body) => Ok(Some(serde_json::from_str(&body)?)),
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
      Err(error) => Err(error.into()),
    }
  }

  /// Write atomically (temp file in the same directory, then `rename`), so a
  /// concurrent reader sees the old or the new document, never a torn one. A
  /// failed write or rename removes the temp file (best effort, WARN).
  ///
  /// # Errors
  ///
  /// [`RunnerError::Json`] when it cannot be encoded,
  /// [`RunnerError::WorkspaceInit`] when the file cannot be written.
  pub fn save(&self, path: &Path) -> Result<(), RunnerError> {
    let body = serde_json::to_vec_pretty(self)?;
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, body)
      .and_then(|()| std::fs::rename(&temp, path))
      .map_err(|source| {
        if let Err(error) = std::fs::remove_file(&temp)
          && error.kind() != std::io::ErrorKind::NotFound
        {
          tracing::warn!(path = %temp.display(), error = %error, "cannot remove temp tracking file");
        }
        RunnerError::WorkspaceInit {
          path: path.to_path_buf(),
          source,
        }
      })
  }
}

/// Every tracking file under `<work_root>/_PipelineMapping/*/*/` (per-file
/// read errors are logged and skipped; a missing mapping dir is empty).
pub fn tracking_files(work_root: &Path) -> Vec<PathBuf> {
  let mut files = Vec::new();
  for owner in child_dirs(&work_root.join(MAPPING_DIR)) {
    if owner.file_name().is_some_and(|name| name == JOB_LEASES_DIR) {
      continue;
    }
    for repo in child_dirs(&owner) {
      let file = repo.join(TRACKING_FILE);
      if file.is_file() {
        files.push(file);
      }
    }
  }
  files
}

/// Current Unix time in seconds (0 if the clock is before the epoch).
pub fn now_unix() -> u64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map_or(0, |elapsed| elapsed.as_secs())
}

fn child_dirs(dir: &Path) -> Vec<PathBuf> {
  match std::fs::read_dir(dir) {
    Ok(entries) => entries
      .filter_map(|entry| match entry {
        Ok(entry) => Some(entry.path()),
        Err(error) => {
          tracing::warn!(dir = %dir.display(), error = %error, "skipping unreadable mapping entry");
          None
        },
      })
      .filter(|path| path.is_dir())
      .collect(),
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
    Err(error) => {
      tracing::warn!(dir = %dir.display(), error = %error, "cannot list workspace mappings");
      Vec::new()
    },
  }
}

/// Exactly one normal path component (no separators, `.`, `..` or root).
pub fn is_single_component(name: &str) -> bool {
  let mut components = Path::new(name).components();
  matches!(components.next(), Some(Component::Normal(_)))
    && components.next().is_none()
    && !name.contains(['/', '\\', '\0'])
}
