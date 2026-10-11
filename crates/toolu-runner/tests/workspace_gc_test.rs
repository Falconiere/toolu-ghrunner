//! Real-data workspace GC tests (issue #90 AC-6 / 90-S4).
//!
//! Workspaces are created by the production `prepare_workspace` from the
//! recorded job message (`tests/fixtures/job_message.json`, only
//! `github.repository` and the job id rewritten) on a real shared work root.
//! Ages are real on-disk state: directory mtimes via `filetime`, stable
//! `lastRunOnUnix` via the real tracking file. Leases are live `fs2` locks
//! held by an unreleased `PreparedWorkspace`, as a running job holds them.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use execution::execution::workspace_gc::{GcPolicy, gc_workspaces};
use execution::execution::workspace_layout::{PreparedWorkspace, prepare_workspace};
use execution::execution::workspace_tracking::{TrackingConfig, now_unix};
use filetime::FileTime;
use shared::{AgentJobRequestMessage, RunnerConfig, WorkspaceConfig, WorkspaceMode};

/// Boxed error alias for test helpers that use `?`.
type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

const HOUR: u64 = 3600;
const FIXTURE: &str = include_str!("fixtures/job_message.json");

/// Create `root/<name>/marker.txt` — a real directory holding a real file.
fn make_job_dir(root: &Path, name: &str) -> TestResult<()> {
  let dir = root.join(name);
  fs::create_dir_all(&dir)?;
  fs::write(dir.join("marker.txt"), name.as_bytes())?;
  Ok(())
}

/// Backdate `path`'s mtime to `secs` seconds before now.
fn age_by(path: &Path, secs: u64) -> TestResult<()> {
  let past = SystemTime::now()
    .checked_sub(Duration::from_secs(secs))
    .ok_or("age underflowed the clock")?;
  filetime::set_file_mtime(path, FileTime::from_system_time(past))?;
  Ok(())
}

fn policy(data_dir: &Path, job_hours: u64, stable_hours: u64) -> GcPolicy {
  let age = |hours: u64| (hours > 0).then(|| Duration::from_secs(hours * HOUR));
  GcPolicy {
    data_dir: data_dir.to_path_buf(),
    job_max_age: age(job_hours),
    stable_max_age: age(stable_hours),
  }
}

/// A registration on the shared work root `root/work`.
fn registration(root: &Path, name: &str, mode: WorkspaceMode) -> RunnerConfig {
  RunnerConfig {
    data_dir: root.join("registrations").join(name),
    workspace_root: root.join("work"),
    workspace: WorkspaceConfig {
      mode,
      stable_gc_hours: 0,
    },
    ..RunnerConfig::default()
  }
}

/// The recorded job message for `repository` with job id `job_id`.
fn message(repository: &str, job_id: &str) -> TestResult<AgentJobRequestMessage> {
  let mut msg: AgentJobRequestMessage = serde_json::from_str(FIXTURE)?;
  job_id.clone_into(&mut msg.job_id);
  msg.workspace = None;
  let github = msg
    .context_data
    .get_mut("github")
    .and_then(|github| github.d.as_mut())
    .ok_or("fixture github context")?;
  let entry = github
    .iter_mut()
    .find(|entry| entry.key.s.as_deref() == Some("repository"))
    .ok_or("fixture github.repository")?;
  entry.value.s = Some(repository.to_owned());
  Ok(msg)
}

fn prepare(config: &RunnerConfig, repository: &str, job_id: &str) -> TestResult<PreparedWorkspace> {
  Ok(prepare_workspace(config, &message(repository, job_id)?)?)
}

/// Rewrite a stable workspace's last use to `hours` ago.
fn last_used(work: &Path, repository: &str, hours: u64) -> TestResult<PathBuf> {
  let file = work
    .join("_PipelineMapping")
    .join(repository)
    .join("PipelineFolder.json");
  let mut tracking = TrackingConfig::load(&file)?.ok_or("tracking file")?;
  tracking.last_run_on_unix = now_unix() - hours * HOUR;
  tracking.save(&file)?;
  Ok(file)
}

#[test]
fn prunes_old_dirs_keeps_fresh_and_running() -> TestResult<()> {
  let tmp = tempfile::tempdir()?;
  let root = tmp.path();

  // Pre-#90 directories (no lease file): two stale dirs to reap, one stale
  // dir that is the running job (must live), and two fresh dirs.
  for name in [
    "job-old-a",
    "job-old-b",
    "job-keep",
    "job-fresh-a",
    "job-fresh-b",
  ] {
    make_job_dir(root, name)?;
  }
  for old in ["job-old-a", "job-old-b", "job-keep"] {
    age_by(&root.join(old), 48 * HOUR)?;
  }

  let removed = gc_workspaces(root, &policy(root, 24, 0), "job-keep")?;

  assert_eq!(
    removed, 2,
    "exactly the two non-kept stale dirs are removed"
  );
  assert!(!root.join("job-old-a").exists(), "stale dir a is reaped");
  assert!(!root.join("job-old-b").exists(), "stale dir b is reaped");
  assert!(
    root.join("job-keep").exists(),
    "the running job survives despite being old"
  );
  assert!(root.join("job-fresh-a").exists(), "fresh dir a survives");
  assert!(root.join("job-fresh-b").exists(), "fresh dir b survives");
  Ok(())
}

#[test]
fn missing_root_is_ok_zero() -> TestResult<()> {
  let tmp = tempfile::tempdir()?;
  let missing = tmp.path().join("does-not-exist");

  let removed = gc_workspaces(&missing, &policy(tmp.path(), 24, 24), "job-keep")?;

  assert_eq!(removed, 0, "a missing workspace root prunes nothing");
  Ok(())
}

#[test]
fn plain_file_in_root_is_left_untouched() -> TestResult<()> {
  let tmp = tempfile::tempdir()?;
  let root = tmp.path();

  let stray = root.join("stray.txt");
  fs::write(&stray, b"not a job dir")?;
  age_by(&stray, 48 * HOUR)?;
  make_job_dir(root, "job-old")?;
  age_by(&root.join("job-old"), 48 * HOUR)?;

  let removed = gc_workspaces(root, &policy(root, 24, 0), "job-keep")?;

  assert_eq!(removed, 1, "only the stale directory is removed");
  assert!(
    stray.exists(),
    "an aged plain file in the root is never removed"
  );
  assert!(
    !root.join("job-old").exists(),
    "the stale directory is removed"
  );
  Ok(())
}

#[test]
fn stable_gc_prunes_only_owned_stale_unleased_workspaces() -> TestResult<()> {
  let tmp = tempfile::tempdir()?;
  let a = registration(tmp.path(), "a", WorkspaceMode::Stable);
  let b = registration(tmp.path(), "b", WorkspaceMode::Stable);
  let work = a.workspace_root.clone();

  drop(prepare(&a, "alice/app", "j1")?);
  drop(prepare(&a, "alice/lib", "j2")?);
  drop(prepare(&b, "bob/tool", "j3")?);
  drop(prepare(&a, "alice/current", "j4")?);
  let held = prepare(&a, "alice/held", "j5")?;
  let stale_app = last_used(&work, "alice/app", 100)?;
  last_used(&work, "alice/lib", 1)?;
  last_used(&work, "bob/tool", 100)?;
  last_used(&work, "alice/current", 100)?;
  last_used(&work, "alice/held", 100)?;
  // Stable directories are not mtime candidates, however old.
  for dir in ["app", "lib", "tool", "current", "held"] {
    age_by(&work.join(dir), 500 * HOUR)?;
  }

  // The default (stable_gc_hours = 0) never prunes a stable workspace.
  assert_eq!(
    gc_workspaces(&work, &policy(&a.data_dir, 24, 0), "current")?,
    0
  );

  let removed = gc_workspaces(&work, &policy(&a.data_dir, 24, 48), "current")?;

  assert_eq!(
    removed, 1,
    "only alice/app is owned, stale, unleased, not kept"
  );
  assert!(!work.join("app").exists(), "owned stale workspace pruned");
  assert!(!stale_app.exists(), "its tracking file goes with it");
  assert!(
    work.join("lib/lib").exists(),
    "recently used workspace kept"
  );
  assert!(
    work.join("tool/tool").exists(),
    "another registration's workspace kept"
  );
  assert!(
    work.join("current/current").exists(),
    "the running job's workspace kept"
  );
  assert!(
    work.join("held/held").exists(),
    "a leased (active) workspace kept"
  );
  assert!(
    work.join("_PipelineMapping").is_dir(),
    "mapping root never deleted"
  );
  drop(held);

  // A pruned repository gets its stable name back on the next job.
  let again = prepare(&a, "alice/app", "j6")?;
  assert_eq!(again.workspace, work.join("app/app"));
  Ok(())
}

#[test]
fn fresh_gc_respects_owner_and_live_job_leases() -> TestResult<()> {
  let tmp = tempfile::tempdir()?;
  let a = registration(tmp.path(), "a", WorkspaceMode::Fresh);
  let b = registration(tmp.path(), "b", WorkspaceMode::Fresh);
  let work = a.workspace_root.clone();

  drop(prepare(&a, "alice/app", "job-a-old")?);
  drop(prepare(&b, "bob/app", "job-b-old")?);
  let live = prepare(&a, "alice/app", "job-a-live")?;
  drop(prepare(&a, "alice/app", "job-a-new")?);
  make_job_dir(&work, "legacy-job")?;
  // A stable workspace on the same root is claimed, never an mtime candidate.
  let stable = registration(tmp.path(), "a", WorkspaceMode::Stable);
  drop(prepare(&stable, "alice/kept", "job-s")?);
  for dir in ["job-a-old", "job-b-old", "job-a-live", "legacy-job", "kept"] {
    age_by(&work.join(dir), 48 * HOUR)?;
  }
  let jobs = work.join("_PipelineMapping/_jobs");
  age_by(&work.join("_PipelineMapping"), 48 * HOUR)?;

  let removed = gc_workspaces(&work, &policy(&a.data_dir, 24, 0), "job-a-new")?;

  assert_eq!(removed, 2, "job-a-old and the pre-#90 legacy dir");
  assert!(!work.join("job-a-old").exists(), "own stale job dir pruned");
  assert!(!jobs.join("job-a-old.lock").exists(), "with its lease file");
  assert!(
    !work.join("legacy-job").exists(),
    "lease-less legacy dir keeps the mtime rule"
  );
  assert!(
    work.join("job-b-old").exists(),
    "another registration's job dir kept"
  );
  assert!(jobs.join("job-b-old.lock").exists());
  assert!(
    work.join("job-a-live").exists(),
    "a live job's leased dir kept"
  );
  assert!(
    work.join("job-a-new").exists(),
    "the running job's dir kept"
  );
  assert!(work.join("kept/kept").exists(), "claimed stable dir kept");
  assert!(work.join("_PipelineMapping").is_dir(), "mapping root kept");
  drop(live);
  Ok(())
}
