//! Garbage collection of job workspaces on a (possibly shared) work root (#90).
//!
//! Stable directories go when their tracking file's `lastRunOnUnix` is older
//! than `stable_max_age` (default never), names this `data_dir`, and the
//! repository lease is free. Fresh `<job_id>` directories go when older than
//! `job_max_age` with a free job lease naming this `data_dir`; one without a
//! lease file predates #90 and keeps the mtime-only rule. `_`-prefixed,
//! claimed and `keep` children are never fresh candidates; an unreadable
//! tracking file claims every name its repository could have been given.
//! Unsure → keep.

use std::fs::{self, DirEntry};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use shared::{RunnerConfig, RunnerError};

use super::workspace_lease::WorkspaceLease;
use super::workspace_tracking::{
  JOB_LEASES_DIR, LEASE_FILE, MAPPING_DIR, TrackingConfig, now_unix, tracking_files,
};

/// Ceiling on a configured age (100 years) so the seconds conversion cannot
/// saturate; a value at or above it means "never prune".
const MAX_GC_HOURS: u64 = 100 * 365 * 24;

/// Which workspaces one registration's sweep may prune.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GcPolicy {
  /// The sweeping registration; only workspaces it last used are pruned.
  pub data_dir: PathBuf,
  /// Fresh per-job directories older than this are pruned; `None` = never.
  pub job_max_age: Option<Duration>,
  /// Stable directories unused for longer than this are pruned; `None` = never.
  pub stable_max_age: Option<Duration>,
}

impl GcPolicy {
  /// The policy `config` asks for, or `None` when it prunes nothing
  /// (`workspace_gc_hours` and `stable_gc_hours` both `0`).
  pub fn from_config(config: &RunnerConfig) -> Option<Self> {
    let policy = Self {
      data_dir: config.data_dir.clone(),
      job_max_age: max_age(config.workspace_gc_hours),
      stable_max_age: max_age(config.workspace.stable_gc_hours),
    };
    (policy.job_max_age.is_some() || policy.stable_max_age.is_some()).then_some(policy)
  }
}

/// `0` disables; larger values are capped at [`MAX_GC_HOURS`].
fn max_age(hours: u64) -> Option<Duration> {
  (hours > 0).then(|| Duration::from_secs(hours.min(MAX_GC_HOURS) * 3600))
}

/// Prune stale workspaces under `workspace_root` per `policy`, sparing the
/// work-root child named `keep`. Returns how many directories were removed.
///
/// Best-effort per entry: a failure is logged (WARN) and skipped. A missing
/// `workspace_root` is `Ok(0)`.
///
/// # Errors
///
/// Returns `RunnerError::Io` if `workspace_root` exists but cannot be listed.
pub fn gc_workspaces(
  workspace_root: &Path,
  policy: &GcPolicy,
  keep: &str,
) -> Result<usize, RunnerError> {
  let entries = match fs::read_dir(workspace_root) {
    Ok(entries) => entries,
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
    Err(e) => return Err(e.into()),
  };
  // One scan of the tracking files serves both passes: the stable pass prunes
  // from it, and whatever it keeps is the claimed set the fresh pass skips.
  let mut claimed = Vec::new();
  let mut unreadable = Vec::new();
  let mut removed = 0usize;
  for file in tracking_files(workspace_root) {
    let Some(tracking) = load_tracking(&file) else {
      // Its pipeline directory is unknown (and may be leased by a running
      // job): protect every `<repo>` / `<repo>_<n>` name it could have.
      if let Some(repo) = file.parent().and_then(Path::file_name) {
        unreadable.push(repo.to_string_lossy().to_ascii_lowercase());
      }
      continue;
    };
    let pruned = policy.stable_max_age.is_some_and(|max_age| {
      prune_stable(
        workspace_root,
        &file,
        &tracking,
        &policy.data_dir,
        max_age,
        keep,
      )
    });
    if pruned {
      removed += 1;
    } else {
      claimed.push(tracking.pipeline_directory.to_ascii_lowercase());
    }
  }
  let Some(max_age) = policy.job_max_age else {
    return Ok(removed);
  };
  let now = SystemTime::now();
  for entry in entries {
    match entry {
      Ok(entry) => {
        let name = entry.file_name().to_string_lossy().into_owned();
        let lower = name.to_ascii_lowercase();
        if name == keep
          || name.starts_with('_')
          || claimed.contains(&lower)
          || unreadable
            .iter()
            .any(|repo| is_pipeline_name_for(&lower, repo))
        {
          continue;
        }
        if prune_job_dir(workspace_root, &entry, &policy.data_dir, now, max_age) {
          removed += 1;
        }
      },
      Err(e) => tracing::warn!(error = %e, "workspace GC: skipping unreadable directory entry"),
    }
  }
  Ok(removed)
}

/// Remove one stale stable workspace (`tracking`, read from `file`) and its
/// tracking file when this registration owns it and can take its lease.
/// `true` only when removed.
fn prune_stable(
  root: &Path,
  file: &Path,
  tracking: &TrackingConfig,
  data_dir: &Path,
  max_age: Duration,
  keep: &str,
) -> bool {
  let Some(repo_dir) = file.parent() else {
    return false;
  };
  let eligible = |tracking: &TrackingConfig| {
    tracking.data_dir == data_dir
      && tracking.pipeline_directory != keep
      && tracking.is_usable_for(&tracking.repository_name)
      && is_stale_unix(tracking.last_run_on_unix, max_age)
  };
  if !eligible(tracking) {
    return false;
  }
  let Ok(Some(lease)) = WorkspaceLease::try_acquire(&repo_dir.join(LEASE_FILE)) else {
    return false;
  };
  // Re-read under the lease: a job may have used the workspace meanwhile.
  let Some(tracking) = load_tracking(file).filter(|tracking| eligible(tracking)) else {
    return false;
  };
  let pipeline = root.join(&tracking.pipeline_directory);
  let removed = remove_tree(&pipeline) && remove_file(file);
  // Released only after the deletion: no job can claim the workspace mid-delete.
  drop(lease);
  removed
}

/// Remove one stale fresh (or pre-#90) job directory. `true` only when removed.
fn prune_job_dir(
  root: &Path,
  entry: &DirEntry,
  data_dir: &Path,
  now: SystemTime,
  max_age: Duration,
) -> bool {
  let lease_path = root.join(MAPPING_DIR).join(JOB_LEASES_DIR).join(format!(
    "{}{LEASE_FILE}",
    entry.file_name().to_string_lossy()
  ));
  if !lease_path.exists() {
    return is_stale_dir(entry, now, max_age) && remove_tree(&entry.path());
  }
  // `lease_owner` is `Option<PathBuf>`; `as_deref` borrows it as
  // `Option<&Path>` to compare with this registration's `data_dir`. An
  // unreadable or ownerless lease is `None`, which never matches: skip it.
  if lease_owner(&lease_path).as_deref() != Some(data_dir) {
    return false;
  }
  let Ok(Some(lease)) = WorkspaceLease::try_acquire(&lease_path) else {
    return false;
  };
  let removed =
    is_stale_dir(entry, now, max_age) && remove_tree(&entry.path()) && remove_file(&lease_path);
  // Released only after the deletion (see `prune_stable`).
  drop(lease);
  removed
}

/// Whether `name` is one `new_tracking` could allocate for `repo`: `<repo>`
/// or `<repo>_<n>`. Both are lowercase.
fn is_pipeline_name_for(name: &str, repo: &str) -> bool {
  name.strip_prefix(repo).is_some_and(|rest| {
    rest.is_empty()
      || rest
        .strip_prefix('_')
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
  })
}

fn load_tracking(file: &Path) -> Option<TrackingConfig> {
  match TrackingConfig::load(file) {
    Ok(tracking) => tracking,
    Err(e) => {
      tracing::warn!(path = %file.display(), error = %e, "workspace GC: unreadable tracking file, skipping");
      None
    },
  }
}

/// The `dataDir` a fresh job lease records, if readable.
fn lease_owner(path: &Path) -> Option<PathBuf> {
  let body = fs::read_to_string(path).ok()?;
  let value: serde_json::Value = serde_json::from_str(&body).ok()?;
  value.get("dataDir")?.as_str().map(PathBuf::from)
}

/// Whether a Unix timestamp is more than `max_age` in the past. `0` (unknown)
/// and future timestamps are never stale.
fn is_stale_unix(last_run: u64, max_age: Duration) -> bool {
  last_run > 0 && now_unix().saturating_sub(last_run) > max_age.as_secs()
}

/// Whether `entry` is a directory whose mtime is more than `max_age` before
/// `now`. Unreadable metadata, an unavailable mtime, or a future mtime (clock
/// skew) counts as fresh: never delete when unsure.
fn is_stale_dir(entry: &DirEntry, now: SystemTime, max_age: Duration) -> bool {
  let metadata = match entry.metadata() {
    Ok(metadata) => metadata,
    Err(e) => {
      tracing::warn!(path = %entry.path().display(), error = %e, "workspace GC: cannot stat entry, skipping");
      return false;
    },
  };
  metadata.is_dir()
    && metadata
      .modified()
      .ok()
      .and_then(|mtime| now.duration_since(mtime).ok())
      .is_some_and(|age| age > max_age)
}

fn remove_tree(path: &Path) -> bool {
  match fs::remove_dir_all(path) {
    Ok(()) => {
      tracing::debug!(path = %path.display(), "workspace GC: removed stale workspace");
      true
    },
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
    Err(e) => {
      tracing::warn!(path = %path.display(), error = %e, "workspace GC: cannot remove entry, skipping");
      false
    },
  }
}

fn remove_file(path: &Path) -> bool {
  match fs::remove_file(path) {
    Ok(()) => true,
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
    Err(e) => {
      tracing::warn!(path = %path.display(), error = %e, "workspace GC: cannot remove file, skipping");
      false
    },
  }
}
