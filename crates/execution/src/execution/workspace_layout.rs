//! Job workspace allocation (issue #90): stable per-repository or fresh.
//!
//! Stable mode follows upstream `PipelineDirectoryManager.PrepareDirectory`
//! (actions/runner `cab9d1c`): `owner/repo` maps to `<work>/<repo>/<repo>`
//! through `_PipelineMapping/<owner>/<repo>/PipelineFolder.json`, and
//! `workspace.clean` (`all` / `resources` / `outputs`) is applied as upstream
//! does. Fresh mode — and the fallback when the repository is unknown or its
//! stable workspace is leased by another live job — keeps the pre-#90
//! `<work>/<job_id>` layout (`runner.workspace` is the same directory).
//!
//! Two phases keep setup failures side-effect free on the workspace:
//! [`prepare_workspace`] claims (paths, the stable lease and tracking file)
//! before the job context exists, and [`PreparedWorkspace::create_dirs`]
//! creates directories only after the job's container/services/environment
//! evaluated. Every job holds a [`WorkspaceLease`] until it drops the
//! [`PreparedWorkspace`].

use std::path::{Path, PathBuf};

use shared::paths::sanitize_job_id;
use shared::{AgentJobRequestMessage, RunnerConfig, RunnerError, WorkspaceMode};

use super::workspace_lease::WorkspaceLease;
use super::workspace_tracking::{
  JOB_LEASES_DIR, LEASE_FILE, MAPPING_DIR, TRACKING_FILE, TrackingConfig, is_single_component,
  now_unix, tracking_files,
};

/// Upper bound on `<repo>_<n>` collision suffixes before giving up.
const MAX_SUFFIX: usize = 1000;

/// Which layout a job's workspace ended up in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceKind {
  /// The repository's reused `<work>/<pipeline>/<repo>`.
  Stable,
  /// A new `<work>/<job_id>` for this job only.
  Fresh,
}

/// A job's claimed directories plus the lease that keeps them its own.
#[derive(Debug)]
pub struct PreparedWorkspace {
  /// `github.workspace` / `GITHUB_WORKSPACE`.
  pub workspace: PathBuf,
  /// `runner.workspace` / `RUNNER_WORKSPACE` (the work-root child).
  pub pipeline: PathBuf,
  /// Stable or fresh.
  pub kind: WorkspaceKind,
  /// The work-root child (`pipeline`'s name) GC must spare while this job
  /// runs: the validated tracking `pipelineDirectory`, or the job directory.
  pub pipeline_name: String,
  /// Stable: upstream `workspace.clean`, applied by `create_dirs`.
  clean: Option<String>,
  /// Fresh: the job lease file `create_dirs` takes, and its owner body.
  job_lease: Option<(PathBuf, PathBuf)>,
  lease: Option<WorkspaceLease>,
}

impl PreparedWorkspace {
  /// Create the claimed directories. Stable: apply `workspace.clean`, then
  /// ensure the workspace exists. Fresh: take the job lease (recording this
  /// registration's `data_dir`), then create `<work>/<job_id>`.
  ///
  /// # Errors
  ///
  /// [`RunnerError::WorkspaceInit`] when a lease, deletion or directory fails.
  pub fn create_dirs(&mut self) -> Result<(), RunnerError> {
    if let Some((lease_path, data_dir)) = self.job_lease.take() {
      self.lease = Some(take_job_lease(&lease_path, &data_dir)?);
    }
    apply_clean(self.clean.as_deref(), &self.pipeline, &self.workspace)
  }
}

/// `owner/name` from the job's `github.repository`, validated as path parts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repository {
  /// Repository owner (`github.repository_owner`).
  pub owner: String,
  /// Repository name.
  pub name: String,
}

impl Repository {
  /// Parse `owner/name`; `None` for anything that is not exactly two safe
  /// path components (an owner may not start with `_`, which GitHub forbids
  /// and `_PipelineMapping/_jobs` reserves).
  pub fn parse(full_name: &str) -> Option<Self> {
    let (owner, name) = full_name.split_once('/')?;
    (is_single_component(owner) && is_single_component(name) && !owner.starts_with('_')).then(
      || Self {
        owner: owner.to_owned(),
        name: name.to_owned(),
      },
    )
  }

  fn full_name(&self) -> String {
    format!("{}/{}", self.owner, self.name)
  }
}

/// Claim the job's workspace: its paths and, in stable mode, the repository
/// lease and tracking file. Creates no workspace directory (see
/// [`PreparedWorkspace::create_dirs`]).
///
/// # Errors
///
/// [`RunnerError::WorkspaceInit`] when the tracking file or a lease cannot be
/// written.
pub fn prepare_workspace(
  config: &RunnerConfig,
  msg: &AgentJobRequestMessage,
) -> Result<PreparedWorkspace, RunnerError> {
  if config.workspace.mode == WorkspaceMode::Stable
    && let Some(repository) = github_repository(msg)
  {
    let clean = msg.workspace.as_ref().and_then(|w| w.clean.clone());
    if let Some(prepared) = prepare_stable(config, &repository, clean)? {
      return Ok(prepared);
    }
    tracing::warn!(
      repository = %repository.full_name(),
      "stable workspace is in use by another job; using a fresh workspace"
    );
  }
  Ok(prepare_fresh(config, &msg.job_id))
}

/// `github.repository` from the message's context data, if usable.
fn github_repository(msg: &AgentJobRequestMessage) -> Option<Repository> {
  let value = msg
    .context_data
    .get("github")
    .and_then(|github| github.d.as_ref())
    .into_iter()
    .flatten()
    .find(|entry| entry.key.s.as_deref() == Some("repository"))
    .and_then(|entry| entry.value.s.clone());
  let parsed = value.as_deref().and_then(Repository::parse);
  if parsed.is_none() {
    tracing::warn!(repository = ?value, "github.repository is absent or not a safe owner/name; using a fresh workspace");
  }
  parsed
}

/// The repository's stable workspace, or `None` when another job leases it.
fn prepare_stable(
  config: &RunnerConfig,
  repository: &Repository,
  clean: Option<String>,
) -> Result<Option<PreparedWorkspace>, RunnerError> {
  let work = &config.workspace_root;
  let mapping_root = work.join(MAPPING_DIR);
  let root_lock = WorkspaceLease::acquire(&mapping_root.join(LEASE_FILE))?;
  let repo_dir = mapping_root.join(&repository.owner).join(&repository.name);
  let Some(lease) = WorkspaceLease::try_acquire(&repo_dir.join(LEASE_FILE))? else {
    return Ok(None);
  };
  let tracking_path = repo_dir.join(TRACKING_FILE);
  let full_name = repository.full_name();
  let mut tracking = match TrackingConfig::load(&tracking_path) {
    Ok(Some(tracking)) if tracking.is_usable_for(&full_name) => tracking,
    Ok(found) => {
      if found.is_some() {
        tracing::warn!(path = %tracking_path.display(), "tracking file names other directories; recreating it");
      }
      new_tracking(work, repository)?
    },
    Err(error) => {
      tracing::warn!(path = %tracking_path.display(), error = %error, "unreadable tracking file; recreating it");
      new_tracking(work, repository)?
    },
  };
  tracking.last_run_on_unix = now_unix();
  tracking.data_dir.clone_from(&config.data_dir);
  tracking.save(&tracking_path)?;
  drop(root_lock);

  Ok(Some(PreparedWorkspace {
    workspace: work.join(&tracking.workspace_directory),
    pipeline: work.join(&tracking.pipeline_directory),
    kind: WorkspaceKind::Stable,
    pipeline_name: tracking.pipeline_directory,
    clean,
    job_lease: None,
    lease: Some(lease),
  }))
}

/// A mapping for a repository seen for the first time. The pipeline directory
/// is `<repo>`, else the first free `<repo>_<n>` (n ≥ 2) that no other mapping
/// claims and that does not already exist (a legacy per-job directory, or a
/// directory whose mapping was lost). Called under the work-root lock.
fn new_tracking(work: &Path, repository: &Repository) -> Result<TrackingConfig, RunnerError> {
  let claimed: Vec<String> = tracking_files(work)
    .iter()
    .filter_map(|file| TrackingConfig::load(file).ok().flatten())
    .map(|tracking| tracking.pipeline_directory.to_ascii_lowercase())
    .collect();
  let name = &repository.name;
  let candidates =
    std::iter::once(name.clone()).chain((2..=MAX_SUFFIX).map(|n| format!("{name}_{n}")));
  for candidate in candidates {
    let lower = candidate.to_ascii_lowercase();
    if lower == MAPPING_DIR.to_ascii_lowercase()
      || claimed.contains(&lower)
      || work.join(&candidate).exists()
    {
      continue;
    }
    return Ok(TrackingConfig::new(
      &repository.full_name(),
      name,
      &candidate,
    ));
  }
  Err(RunnerError::WorkspaceInit {
    path: work.join(name),
    source: std::io::Error::other("no free pipeline directory name"),
  })
}

/// Upstream `workspace.clean`, case-insensitive: `all` recreates the pipeline
/// and workspace directories, `resources` recreates the workspace (the one
/// tracked repository), `outputs` deletes the pipeline directory's other
/// subdirectories. Anything else only ensures both exist.
fn apply_clean(clean: Option<&str>, pipeline: &Path, workspace: &Path) -> Result<(), RunnerError> {
  match clean.map(str::to_ascii_lowercase).as_deref() {
    Some("all") => remove_dir(pipeline)?,
    Some("resources") => remove_dir(workspace)?,
    Some("outputs") => {
      for entry in std::fs::read_dir(pipeline).map_err(|source| init_error(pipeline, source))? {
        let path = entry.map_err(|source| init_error(pipeline, source))?.path();
        if path != workspace && path.is_dir() {
          remove_dir(&path)?;
        }
      }
    },
    _ => {},
  }
  std::fs::create_dir_all(workspace).map_err(|source| init_error(workspace, source))
}

/// A new `<work>/<job_id>`; its job lease is taken by `create_dirs`.
fn prepare_fresh(config: &RunnerConfig, job_id: &str) -> PreparedWorkspace {
  let job_dir = sanitize_job_id(job_id);
  let lease_path = config
    .workspace_root
    .join(MAPPING_DIR)
    .join(JOB_LEASES_DIR)
    .join(format!("{job_dir}{LEASE_FILE}"));
  let workspace = config.workspace_root.join(&job_dir);
  PreparedWorkspace {
    pipeline: workspace.clone(),
    workspace,
    kind: WorkspaceKind::Fresh,
    pipeline_name: job_dir,
    clean: None,
    job_lease: Some((lease_path, config.data_dir.clone())),
    lease: None,
  }
}

/// Take a fresh job's lease and record its owner (`{"dataDir": …}`).
fn take_job_lease(lease_path: &Path, data_dir: &Path) -> Result<WorkspaceLease, RunnerError> {
  let mut lease = WorkspaceLease::try_acquire(lease_path)?.ok_or_else(|| {
    init_error(
      lease_path,
      std::io::Error::other("job workspace lease already held"),
    )
  })?;
  let body = serde_json::to_vec(&serde_json::json!({ "dataDir": data_dir }))?;
  if let Err(error) = lease.write_body(&body) {
    // The directory is not created yet; drop the ownerless lease file too
    // (job ids are unique, so nobody else can be waiting on it).
    if let Err(remove) = std::fs::remove_file(lease_path) {
      tracing::warn!(path = %lease_path.display(), error = %remove, "cannot remove job lease file");
    }
    return Err(error);
  }
  Ok(lease)
}

fn remove_dir(path: &Path) -> Result<(), RunnerError> {
  match std::fs::remove_dir_all(path) {
    Ok(()) => Ok(()),
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
    Err(source) => Err(init_error(path, source)),
  }
}

fn init_error(path: &Path, source: std::io::Error) -> RunnerError {
  RunnerError::WorkspaceInit {
    path: path.to_path_buf(),
    source,
  }
}

#[cfg(test)]
#[path = "tests/workspace_layout.rs"]
mod tests;
