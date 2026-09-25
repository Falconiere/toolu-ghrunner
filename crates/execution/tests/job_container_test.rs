//! Automatic entrypoint checks: host dispatch plus target-specific rejection/cancellation.

use std::sync::{Arc, Mutex};

use execution::Runner;
use shared::{
  ActionStep, AgentJobRequestMessage, Conclusion, RunnerConfig, RunnerEvent, SecretMasker,
};
use tokio_util::sync::CancellationToken;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn message() -> Result<AgentJobRequestMessage, serde_json::Error> {
  serde_json::from_str(include_str!(
    "../../toolu-runner/tests/fixtures/job_message.json"
  ))
}

#[tokio::test]
async fn job_container_absent_preserves_host_execution() -> TestResult {
  let root = tempfile::tempdir()?;
  let config = RunnerConfig {
    data_dir: root.path().join("data"),
    workspace_root: root.path().join("work"),
    ..RunnerConfig::default()
  };
  let mut job = message()?;
  let workspace = config.workspace_root.join(&job.job_id);
  job.steps = vec![ActionStep::script("host", "printf host > marker", "")];
  let runner = Runner::new(config, Arc::new(Mutex::new(SecretMasker::new())));
  let mut events = runner.execute_job(job, CancellationToken::new());
  let mut conclusion = None;
  while let Some(event) = events.recv().await {
    if let RunnerEvent::JobCompleted {
      conclusion: result, ..
    } = event
    {
      conclusion = Some(result);
    }
  }
  assert_eq!(conclusion, Some(Conclusion::Success));
  assert_eq!(std::fs::read_to_string(workspace.join("marker"))?, "host");
  Ok(())
}

#[cfg(not(target_os = "linux"))]
#[tokio::test]
async fn job_container_non_linux_fails_before_workspace_or_step() -> TestResult {
  let root = tempfile::tempdir()?;
  let config = RunnerConfig {
    data_dir: root.path().join("data"),
    workspace_root: root.path().join("work"),
    ..RunnerConfig::default()
  };
  let mut job = message()?;
  let workspace = config.workspace_root.join(&job.job_id);
  job.job_container = Some(shared::TemplateToken {
    token_type: 0,
    lit: Some("ubuntu:24.04".to_owned()),
    ..Default::default()
  });
  job.steps = vec![ActionStep::script(
    "forbidden",
    "printf forbidden > marker",
    "",
  )];
  let runner = Runner::new(config, Arc::new(Mutex::new(SecretMasker::new())));
  let mut events = runner.execute_job(job, CancellationToken::new());
  let mut conclusion = None;
  let mut logs = Vec::new();
  while let Some(event) = events.recv().await {
    match event {
      RunnerEvent::JobCompleted {
        conclusion: result, ..
      } => conclusion = Some(result),
      RunnerEvent::Log { line, .. } => logs.push(line),
      RunnerEvent::JobStarted { .. }
      | RunnerEvent::StepStarted { .. }
      | RunnerEvent::StepCompleted { .. }
      | RunnerEvent::StepSkipped { .. }
      | RunnerEvent::LogGroup { .. }
      | RunnerEvent::Annotation { .. } => {},
    }
  }
  assert_eq!(conclusion, Some(Conclusion::Failure));
  assert!(logs.iter().any(|line| line.contains("Linux")), "{logs:?}");
  assert!(
    !workspace.exists(),
    "container job created a host workspace"
  );
  Ok(())
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn job_container_setup_cancellation_reports_cancelled() -> TestResult {
  let root = tempfile::tempdir()?;
  let config = RunnerConfig {
    data_dir: root.path().join("data"),
    workspace_root: root.path().join("work"),
    ..RunnerConfig::default()
  };
  let mut job = message()?;
  let workspace = config.workspace_root.join(&job.job_id);
  job.job_container = Some(shared::TemplateToken {
    token_type: 0,
    lit: Some("ubuntu:24.04".to_owned()),
    ..Default::default()
  });
  job.steps = vec![ActionStep::script(
    "forbidden",
    "printf forbidden > marker",
    "",
  )];
  let cancel = CancellationToken::new();
  cancel.cancel();
  let runner = Runner::new(config, Arc::new(Mutex::new(SecretMasker::new())));
  let mut events = runner.execute_job(job, cancel);
  let mut conclusion = None;
  while let Some(event) = events.recv().await {
    if let RunnerEvent::JobCompleted {
      conclusion: result, ..
    } = event
    {
      conclusion = Some(result);
    }
  }
  assert_eq!(conclusion, Some(Conclusion::Cancelled));
  assert!(!workspace.join("marker").exists());
  Ok(())
}
