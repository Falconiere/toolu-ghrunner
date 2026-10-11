//! `workspace_layout` boundaries on a real temp work root (issue #90).
//!
//! The captured #68 acquisition supplies the message; only
//! `github.repository` and the job id are rewritten. Leases are real `fs2`
//! locks held by a second `WorkspaceLease`, as another job would hold them.

use std::path::Path;

use shared::{
  AgentJobRequestMessage, PipelineContextData, RunnerConfig, WorkspaceConfig, WorkspaceMode,
};

use super::{Repository, WorkspaceKind, prepare_workspace};
use crate::execution::workspace_lease::WorkspaceLease;
use crate::execution::workspace_tracking::TrackingConfig;

const CAPTURE: &str = include_str!("../../../tests/incoming_contexts_matrix_0.json");

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn message(
  repository: Option<&str>,
  job_id: &str,
) -> Result<AgentJobRequestMessage, Box<dyn std::error::Error>> {
  let mut msg: AgentJobRequestMessage = serde_json::from_str(CAPTURE)?;
  job_id.clone_into(&mut msg.job_id);
  let github = msg
    .context_data
    .get_mut("github")
    .and_then(|github| github.d.as_mut())
    .ok_or("captured github context absent")?;
  github.retain(|entry| entry.key.s.as_deref() != Some("repository"));
  if let Some(repository) = repository {
    let mut entry = github.first().ok_or("captured github entry")?.clone();
    entry.key = PipelineContextData::string("repository".to_owned());
    entry.value = PipelineContextData::string(repository.to_owned());
    github.push(entry);
  }
  Ok(msg)
}

fn config(root: &Path, mode: WorkspaceMode) -> RunnerConfig {
  RunnerConfig {
    data_dir: root.join("data"),
    workspace_root: root.join("work"),
    workspace: WorkspaceConfig {
      mode,
      stable_gc_hours: 0,
    },
    ..RunnerConfig::default()
  }
}

#[test]
fn repository_parse_accepts_only_two_safe_components() {
  assert_eq!(
    Repository::parse("octocat/hello-world"),
    Some(Repository {
      owner: "octocat".to_owned(),
      name: "hello-world".to_owned(),
    })
  );
  assert!(Repository::parse("octocat/.github").is_some());
  for bad in [
    "", "octocat", "/x", "x/", "../x", "x/..", "x/.", "a/b/c", "_jobs/x", "a\\b/c", "a/b\0",
  ] {
    assert_eq!(Repository::parse(bad), None, "{bad:?}");
  }
}

#[test]
fn stable_layout_reuses_one_mapping_and_records_the_owner() -> TestResult {
  let root = tempfile::tempdir()?;
  let config = config(root.path(), WorkspaceMode::Stable);
  let first = prepare_workspace(&config, &message(Some("octocat/hello-world"), "job-1")?)?;
  let work = root.path().join("work");
  assert_eq!(first.kind, WorkspaceKind::Stable);
  assert_eq!(first.workspace, work.join("hello-world/hello-world"));
  assert_eq!(first.pipeline, work.join("hello-world"));
  assert_eq!(first.pipeline_name, "hello-world");
  drop(first);
  let second = prepare_workspace(&config, &message(Some("octocat/hello-world"), "job-2")?)?;
  assert_eq!(second.workspace, work.join("hello-world/hello-world"));
  let tracking =
    TrackingConfig::load(&work.join("_PipelineMapping/octocat/hello-world/PipelineFolder.json"))?
      .ok_or("tracking file written")?;
  assert_eq!(tracking.data_dir, config.data_dir);
  assert!(tracking.last_run_on_unix > 0);
  assert!(!work.join("job-1").exists() && !work.join("job-2").exists());
  Ok(())
}

#[test]
fn busy_lease_falls_back_to_a_fresh_workspace() -> TestResult {
  let root = tempfile::tempdir()?;
  let config = config(root.path(), WorkspaceMode::Stable);
  let holder = prepare_workspace(&config, &message(Some("octocat/hello-world"), "job-1")?)?;
  let mut other = prepare_workspace(&config, &message(Some("octocat/hello-world"), "job-2")?)?;
  other.create_dirs()?;
  let work = root.path().join("work");
  assert_eq!(other.kind, WorkspaceKind::Fresh);
  assert_eq!(other.workspace, work.join("job-2"));
  assert_eq!(other.pipeline, work.join("job-2"));
  assert_eq!(holder.kind, WorkspaceKind::Stable);
  // The fresh job's lease records its registration.
  let body = std::fs::read_to_string(work.join("_PipelineMapping/_jobs/job-2.lock"))?;
  let body: serde_json::Value = serde_json::from_str(&body)?;
  assert_eq!(
    body.pointer("/dataDir").and_then(serde_json::Value::as_str),
    Some(config.data_dir.to_string_lossy().as_ref())
  );
  Ok(())
}

#[test]
fn absent_or_unsafe_repository_uses_a_sanitized_fresh_job_dir() -> TestResult {
  let root = tempfile::tempdir()?;
  let config = config(root.path(), WorkspaceMode::Stable);
  let work = root.path().join("work");
  for (n, repository) in [None, Some("../escape"), Some("a/b/c")]
    .into_iter()
    .enumerate()
  {
    let job_id = format!("id/with:odd{n}");
    let prepared = prepare_workspace(&config, &message(repository, &job_id)?)?;
    assert_eq!(prepared.kind, WorkspaceKind::Fresh, "{repository:?}");
    assert_eq!(
      prepared.workspace,
      work.join(format!("id_with_odd{n}")),
      "{repository:?}"
    );
    assert_eq!(prepared.pipeline, prepared.workspace);
  }
  assert!(!root.path().join("escape").exists());
  Ok(())
}

#[test]
fn fresh_mode_never_touches_the_mapping() -> TestResult {
  let root = tempfile::tempdir()?;
  let config = config(root.path(), WorkspaceMode::Fresh);
  let prepared = prepare_workspace(&config, &message(Some("octocat/hello-world"), "job-9")?)?;
  let work = root.path().join("work");
  assert_eq!(prepared.kind, WorkspaceKind::Fresh);
  assert_eq!(prepared.workspace, work.join("job-9"));
  assert_eq!(prepared.pipeline, prepared.workspace);
  assert!(!work.join("_PipelineMapping/octocat").exists());
  Ok(())
}

#[test]
fn corrupt_or_escaping_tracking_file_is_recreated_inside_the_work_root() -> TestResult {
  let root = tempfile::tempdir()?;
  let config = config(root.path(), WorkspaceMode::Stable);
  let work = root.path().join("work");
  let mapping = work.join("_PipelineMapping/octocat/hello-world/PipelineFolder.json");
  let escaping = serde_json::json!({
    "repositoryName": "octocat/hello-world",
    "pipelineDirectory": "..",
    "workspaceDirectory": "../outside",
  });
  for body in [b"not json".to_vec(), serde_json::to_vec(&escaping)?] {
    std::fs::create_dir_all(mapping.parent().ok_or("parent")?)?;
    std::fs::write(&mapping, &body)?;
    let prepared = prepare_workspace(&config, &message(Some("octocat/hello-world"), "job-1")?)?;
    assert_eq!(prepared.kind, WorkspaceKind::Stable);
    assert!(
      prepared.workspace.starts_with(&work),
      "{:?}",
      prepared.workspace
    );
    let tracking = TrackingConfig::load(&mapping)?.ok_or("recreated")?;
    assert!(tracking.is_usable_for("octocat/hello-world"));
  }
  assert!(!root.path().join("outside").exists());
  Ok(())
}

#[test]
fn reserved_existing_and_claimed_names_get_a_suffix() -> TestResult {
  let root = tempfile::tempdir()?;
  let config = config(root.path(), WorkspaceMode::Stable);
  let work = root.path().join("work");
  // A legacy per-job directory that happens to carry the repository's name.
  std::fs::create_dir_all(work.join("legacy"))?;
  let legacy = prepare_workspace(&config, &message(Some("octocat/legacy"), "job-1")?)?;
  assert_eq!(legacy.pipeline, work.join("legacy_2"));
  let reserved = prepare_workspace(
    &config,
    &message(Some("octocat/_PipelineMapping"), "job-2")?,
  )?;
  assert_eq!(
    reserved.workspace,
    work.join("_PipelineMapping_2/_PipelineMapping")
  );
  let alice = prepare_workspace(&config, &message(Some("alice/app"), "job-3")?)?;
  let bob = prepare_workspace(&config, &message(Some("bob/app"), "job-4")?)?;
  assert_eq!(alice.pipeline, work.join("app"));
  assert_eq!(bob.pipeline, work.join("app_2"));
  assert_eq!(bob.workspace, work.join("app_2/app"));
  Ok(())
}

#[test]
fn a_held_job_lease_is_not_taken_twice() -> TestResult {
  let root = tempfile::tempdir()?;
  let path = root.path().join("lease/.lock");
  let held = WorkspaceLease::try_acquire(&path)?.ok_or("first lease")?;
  assert!(WorkspaceLease::try_acquire(&path)?.is_none());
  drop(held);
  assert!(WorkspaceLease::try_acquire(&path)?.is_some());
  Ok(())
}

#[test]
fn claim_creates_no_workspace_until_create_dirs() -> TestResult {
  let root = tempfile::tempdir()?;
  let work = root.path().join("work");
  for mode in [WorkspaceMode::Stable, WorkspaceMode::Fresh] {
    let config = config(root.path(), mode);
    let job_id = format!("job-{mode:?}");
    let mut claimed = prepare_workspace(&config, &message(Some("octocat/hello-world"), &job_id)?)?;
    // A setup error between claim and create must leave no workspace behind.
    assert!(
      !claimed.workspace.exists(),
      "{mode:?} claim created {:?}",
      claimed.workspace
    );
    assert!(
      !work
        .join("_PipelineMapping/_jobs")
        .join(format!("{job_id}.lock"))
        .exists()
    );
    claimed.create_dirs()?;
    assert!(claimed.workspace.is_dir(), "{mode:?}");
  }
  Ok(())
}
