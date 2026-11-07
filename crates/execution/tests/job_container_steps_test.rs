//! #251 D1: a job container is reported as upstream's numbered
//! "Initialize containers" pre-job row and "Stop containers" post row
//! (v2.338.2 `JobExtension.InitializeJob` / `ContainerOperationProvider`).
//!
//! The live paired run 42389172867 recorded both rows for the official
//! runner and neither for toolu. Real Docker, captured job message.

#![cfg(target_os = "linux")]

use std::collections::HashMap;
use std::error::Error;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use execution::Runner;
use shared::{AgentJobRequestMessage, Conclusion, RunnerConfig, RunnerEvent, SecretMasker};
use tokio_util::sync::CancellationToken;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;
const CAPTURE: &str = include_str!("../../toolu-runner/tests/fixtures/job_container_message.json");
/// A digest no registry serves, so the pull fails inside "Initialize containers".
const MISSING_IMAGE: &str =
  "ubuntu@sha256:0000000000000000000000000000000000000000000000000000000000000251";

/// The captured container job reduced to its first shell step; `image`
/// replaces the declared container image when given.
fn container_job(image: Option<&str>) -> TestResult<AgentJobRequestMessage> {
  let mut job: AgentJobRequestMessage = serde_json::from_str(CAPTURE)?;
  job
    .steps
    .retain(|step| step.context_name.as_deref() == Some("shell"));
  if let Some(image) = image {
    let container = job.job_container.as_mut().ok_or("jobContainer absent")?;
    let entry = container
      .d
      .as_mut()
      .and_then(|entries| {
        entries
          .iter_mut()
          .find(|entry| entry.key.to_string_value() == Some("image"))
      })
      .ok_or("container image absent")?;
    entry.value.lit = Some(image.to_owned());
  }
  Ok(job)
}

/// Run `job` and return its rows as (number, name, conclusion), number order,
/// plus the job conclusion.
async fn rows(
  job: AgentJobRequestMessage,
) -> TestResult<(Vec<(u32, String, Option<Conclusion>)>, Option<Conclusion>)> {
  let base = std::env::var_os("TOOLU_CONTAINER_TEST_ROOT")
    .map(std::path::PathBuf::from)
    .ok_or("TOOLU_CONTAINER_TEST_ROOT must name a path shared with Docker")?;
  let root = tempfile::Builder::new()
    .prefix("251 rows ")
    .tempdir_in(base)?;
  let config = RunnerConfig {
    data_dir: root.path().join("data"),
    workspace_root: root.path().join("work"),
    workspace_gc_hours: 0,
    workspace: shared::WorkspaceConfig {
      mode: shared::WorkspaceMode::Fresh,
      stable_gc_hours: 0,
    },
    ..RunnerConfig::default()
  };
  let runner = Runner::new(config, Arc::new(Mutex::new(SecretMasker::new())));
  let mut events = runner.execute_job(job, CancellationToken::new());
  let mut started = Vec::new();
  let mut finished = HashMap::new();
  let mut conclusion = None;
  tokio::time::timeout(Duration::from_secs(240), async {
    while let Some(event) = events.recv().await {
      if let RunnerEvent::StepStarted {
        step_id,
        step_name,
        step_number,
      } = event
      {
        started.push((step_number, step_name, step_id));
      } else if let RunnerEvent::StepCompleted {
        step_id,
        conclusion,
        ..
      } = event
      {
        finished.insert(step_id, conclusion);
      } else if let RunnerEvent::JobCompleted {
        conclusion: result, ..
      } = event
      {
        conclusion = Some(result);
      }
    }
  })
  .await?;
  started.sort_by_key(|(number, _, _)| *number);
  let rows = started
    .into_iter()
    .map(|(number, name, id)| (number, name, finished.get(&id).copied()))
    .collect();
  Ok((rows, conclusion))
}

#[tokio::test]
#[ignore = "requires a Linux runner and real Docker daemon"]
async fn container_rows_bracket_the_workflow_steps() -> TestResult {
  let (rows, conclusion) = rows(container_job(None)?).await?;
  let shape: Vec<_> = rows
    .iter()
    .map(|(number, name, result)| (*number, name.as_str(), *result))
    .collect();
  let [
    (2, "Initialize containers", Some(Conclusion::Success)),
    (3, _, Some(Conclusion::Success)),
    (4, "Stop containers", Some(Conclusion::Success)),
    (5, "Complete job", Some(Conclusion::Success)),
  ] = shape.as_slice()
  else {
    return Err(format!("unexpected rows {shape:?}").into());
  };
  assert_eq!(conclusion, Some(Conclusion::Success));
  Ok(())
}

#[tokio::test]
#[ignore = "requires a Linux runner and real Docker daemon"]
async fn a_failed_container_start_fails_its_row_and_still_stops() -> TestResult {
  let (rows, conclusion) = rows(container_job(Some(MISSING_IMAGE))?).await?;
  let shape: Vec<_> = rows
    .iter()
    .map(|(number, name, result)| (*number, name.as_str(), *result))
    .collect();
  assert_eq!(
    shape,
    [
      (2, "Initialize containers", Some(Conclusion::Failure)),
      (3, "Stop containers", Some(Conclusion::Success)),
    ],
    "no workflow step may run after a failed container start"
  );
  assert_eq!(conclusion, Some(Conclusion::Failure));
  Ok(())
}
