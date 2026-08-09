//! Fix 2 regression: a failing job must not die silently.
//!
//! Before this fix, `Runner::execute_job`'s spawned task swallowed a
//! `run_job` error into a local `tracing::error!` only (never reaches
//! GitHub) and reported `JobCompleted { job_id: String::new(), .. }` — the
//! GitHub job log got no error line at all, and the run never resolved out
//! of "`in_progress`" since it carried no job id.
//!
//! Drives the REAL `Runner::execute_job` (the second test in the workspace
//! to do so, after `job_teardown_order_test.rs`'s teardown-ordering pair) —
//! no mocks. The failure is forced hermetically, with no network: pointing
//! `workspace_root` at a regular file makes `prepare_job_dirs`'s
//! `create_dir_all` fail with a real `RunnerError::Io` before any step runs.

use std::error::Error;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use execution::Runner;
use shared::{
  ActionStep, ActionStepDefinitionReference, AgentJobRequestMessage, Conclusion, RunnerConfig,
  RunnerEvent, SecretMasker, TemplateToken,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const JOB_MESSAGE: &str = include_str!("fixtures/job_message.json");

/// Real pass over this hermetic failure takes single-digit milliseconds; the
/// margin is for a loaded CI box.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(10);

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

/// Drain the event channel to close, collecting every event along the way.
async fn drain(rx: &mut mpsc::Receiver<RunnerEvent>) -> Vec<RunnerEvent> {
  let mut events = Vec::new();
  while let Some(event) = rx.recv().await {
    events.push(event);
  }
  events
}

#[tokio::test]
async fn failing_job_reports_error_log_and_real_job_id() -> TestResult {
  let dir = tempfile::tempdir()?;
  let data_dir = dir.path().join("data");
  std::fs::create_dir_all(&data_dir)?;
  // A regular file where `workspace_root` should be a directory:
  // `prepare_job_dirs`'s `create_dir_all(workspace_root.join(job_id))` fails
  // before any step runs, driving `run_job` to a real `Err` with zero
  // network involved.
  let workspace_root = dir.path().join("not-a-dir");
  std::fs::write(&workspace_root, b"not a directory")?;

  let config = RunnerConfig {
    data_dir,
    workspace_root,
    cgroup_path: None,
    ..RunnerConfig::default()
  };

  let msg: AgentJobRequestMessage = serde_json::from_str(JOB_MESSAGE)?;
  let job_id = msg.job_id.clone();
  assert!(!job_id.is_empty(), "fixture must carry a real job id");

  let runner = Runner::new(config, Arc::new(Mutex::new(SecretMasker::new())));
  let mut rx = runner.execute_job(msg, CancellationToken::new());

  let events = tokio::time::timeout(DRAIN_TIMEOUT, drain(&mut rx))
    .await
    .map_err(|elapsed| format!("engine event channel never closed ({elapsed})"))?;

  let error_line = events.iter().find_map(|e| {
    if let RunnerEvent::Log { line, .. } = e
      && line.starts_with("##[error]")
    {
      return Some(line.clone());
    }
    None
  });
  assert!(
    error_line.is_some(),
    "expected a ##[error]-prefixed log line reaching the job log; events={events:?}"
  );

  let completed = events.iter().find_map(|e| {
    if let RunnerEvent::JobCompleted {
      job_id, conclusion, ..
    } = e
    {
      return Some((job_id.clone(), *conclusion));
    }
    None
  });
  let (reported_job_id, conclusion) = completed.expect("JobCompleted must be emitted");
  assert_eq!(
    reported_job_id, job_id,
    "JobCompleted must carry the real job id, not an empty one"
  );
  assert_eq!(conclusion, Conclusion::Failure);

  Ok(())
}

/// Build an action step whose `uses:` is a local `./path` ref (the whole
/// path lives in `reference.name`, `path` stays `None` — mirrors how the
/// orchestrator reports a local `uses:` step on the wire; see
/// `ActionRef::local_dir`'s doc comment in `actions/resolver.rs`). `ref_type`
/// is deliberately not `"script"` so `is_run_step()` is false and the step
/// goes through `execute_action`, not `run_script_step`.
fn broken_local_action_step(step_id: &str) -> ActionStep {
  ActionStep {
    id: step_id.to_owned(),
    step_type: Some("node20".to_owned()),
    display_name_token: None,
    context_name: Some(step_id.to_owned()),
    condition: None,
    continue_on_error: None,
    timeout_in_minutes: None,
    reference: ActionStepDefinitionReference {
      ref_type: Some("node20".to_owned()),
      image: None,
      name: Some("./broken-action".to_owned()),
      git_ref: None,
      repository_type: Some("Local".to_owned()),
      path: None,
    },
    inputs: TemplateToken::default(),
    environment: None,
  }
}

/// Fix 3 regression: a step that fails MID-step (after `StepStarted`, before
/// any `Conclusion` exists) must still surface its error on its OWN log
/// stream and close out its `StepCompleted`, or the listener's
/// `forward_log_line` never learns which step's uploader to flush — the
/// error text never reaches GitHub's per-step log, and the step stays
/// "in progress" forever in the UI.
///
/// Forced hermetically, no network: the step's `uses:` is `./broken-action`,
/// a real directory under the job workspace that deliberately holds no
/// `action.yml`/`action.yaml`. `resolve_local_action` (`action_exec.rs`)
/// resolves the directory fine, then `read_manifest` fails with
/// `RunnerError::ActionManifest` — AFTER `run_single_step` has already sent
/// `StepStarted`, exactly the production shape the review flagged.
#[tokio::test]
async fn mid_step_failure_reports_log_before_step_completed_before_job_completed() -> TestResult {
  let dir = tempfile::tempdir()?;
  let data_dir = dir.path().join("data");
  let workspace_root = dir.path().join("work");
  std::fs::create_dir_all(&data_dir)?;
  std::fs::create_dir_all(&workspace_root)?;

  let config = RunnerConfig {
    data_dir,
    workspace_root: workspace_root.clone(),
    cgroup_path: None,
    ..RunnerConfig::default()
  };

  let mut msg: AgentJobRequestMessage = serde_json::from_str(JOB_MESSAGE)?;
  let job_id = msg.job_id.clone();
  assert!(!job_id.is_empty(), "fixture must carry a real job id");

  let step_id = "broken-local-step".to_owned();
  msg.steps = vec![broken_local_action_step(&step_id)];

  // The job workspace is `workspace_root/{job_id}`; `run_job`'s
  // `prepare_job_dirs` `create_dir_all`s it (a no-op once it already exists).
  // `./broken-action` resolves under it: a real, empty directory.
  let action_dir = workspace_root.join(&job_id).join("broken-action");
  std::fs::create_dir_all(&action_dir)?;

  let runner = Runner::new(config, Arc::new(Mutex::new(SecretMasker::new())));
  let mut rx = runner.execute_job(msg, CancellationToken::new());

  let events = tokio::time::timeout(DRAIN_TIMEOUT, drain(&mut rx))
    .await
    .map_err(|elapsed| format!("engine event channel never closed ({elapsed})"))?;

  let started_pos = events
    .iter()
    .position(|e| matches!(e, RunnerEvent::StepStarted { step_id: sid, .. } if sid == &step_id));
  let started_msg = format!("expected StepStarted for {step_id}; events={events:?}");
  let started_at = started_pos.expect(&started_msg);

  let log_pos = events.iter().position(|e| {
    if let RunnerEvent::Log {
      step_id: sid, line, ..
    } = e
    {
      sid == &step_id
        && line.starts_with("##[error]")
        && line.contains("no action.yml or action.yaml in")
    } else {
      false
    }
  });
  let log_msg = format!("expected a ##[error] Log for step {step_id}; events={events:?}");
  let log_at = log_pos.expect(&log_msg);

  let completed_pos = events.iter().position(|e| {
    matches!(
      e,
      RunnerEvent::StepCompleted {
        step_id: sid,
        conclusion: Conclusion::Failure,
        ..
      } if sid == &step_id
    )
  });
  let completed_msg =
    format!("expected a Failure StepCompleted for step {step_id}; events={events:?}");
  let completed_at = completed_pos.expect(&completed_msg);

  assert!(
    started_at < log_at,
    "StepStarted must precede the step's error Log; events={events:?}"
  );
  assert!(
    log_at < completed_at,
    "the error Log must reach the listener before StepCompleted flushes/removes its \
     uploader, or the error text never reaches GitHub's per-step log; events={events:?}"
  );

  let job_completed = events.iter().find_map(|e| {
    if let RunnerEvent::JobCompleted {
      job_id, conclusion, ..
    } = e
    {
      return Some((job_id.clone(), *conclusion));
    }
    None
  });
  let (reported_job_id, job_conclusion) = job_completed.expect("JobCompleted must be emitted");
  assert_eq!(
    reported_job_id, job_id,
    "JobCompleted must carry the real job id"
  );
  assert_eq!(job_conclusion, Conclusion::Failure);

  Ok(())
}
