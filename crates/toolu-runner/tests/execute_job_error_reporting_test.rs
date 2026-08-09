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
use shared::{AgentJobRequestMessage, Conclusion, RunnerConfig, RunnerEvent, SecretMasker};
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
