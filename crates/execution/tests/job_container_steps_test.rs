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

/// The captured shell step reshaped: display `name`, `condition`, and a script
/// that creates `sentinel` on whatever filesystem it runs against.
fn sentinel_step(
  template: &shared::ActionStep,
  name: &str,
  condition: &str,
  sentinel: &std::path::Path,
) -> TestResult<shared::ActionStep> {
  let mut step = template.clone();
  step.id = format!("sentinel-{name}");
  step.context_name = None;
  step.condition = Some(condition.to_owned());
  step.display_name_token = Some(literal(name));
  let script = step
    .inputs
    .d
    .as_mut()
    .and_then(|entries| {
      entries
        .iter_mut()
        .find(|entry| entry.key.to_string_value() == Some("script"))
    })
    .ok_or("captured script input absent")?;
  script.value = literal(&format!("touch '{}'", sentinel.display()));
  Ok(step)
}

fn literal(value: &str) -> shared::TemplateToken {
  shared::TemplateToken {
    token_type: 0,
    lit: Some(value.to_owned()),
    ..shared::TemplateToken::default()
  }
}

/// Live paired run 42403405937: after a failed "Initialize containers" the
/// official runner still walked the steps (a `success()` step skipped, an
/// `always()` step attempted and failed without its container), then "Stop
/// containers" and "Complete job". A step that runs must never fall back to
/// the host: the sentinels it would create there must not exist.
#[tokio::test]
#[ignore = "requires a Linux runner and real Docker daemon"]
async fn a_failed_container_start_walks_the_steps_without_touching_the_host() -> TestResult {
  let mut job = container_job(Some(MISSING_IMAGE))?;
  let template = job
    .steps
    .first()
    .ok_or("captured shell step absent")?
    .clone();
  let sentinels = tempfile::tempdir()?;
  let path = |tag: &str| sentinels.path().join(format!("ran-on-host-{tag}"));
  job.steps = vec![
    sentinel_step(&template, "Default", "success()", &path("success"))?,
    sentinel_step(&template, "On failure", "failure()", &path("failure"))?,
    sentinel_step(&template, "Always", "always()", &path("always"))?,
  ];
  let (rows, conclusion) = rows(job).await?;
  let shape: Vec<_> = rows
    .iter()
    .map(|(number, name, result)| (*number, name.as_str(), *result))
    .collect();
  assert_eq!(
    shape,
    [
      (2, "Initialize containers", Some(Conclusion::Failure)),
      (3, "Default", Some(Conclusion::Skipped)),
      (4, "On failure", Some(Conclusion::Failure)),
      (5, "Always", Some(Conclusion::Failure)),
      (6, "Stop containers", Some(Conclusion::Success)),
      (7, "Complete job", Some(Conclusion::Success)),
    ]
  );
  assert_eq!(conclusion, Some(Conclusion::Failure));
  for tag in ["success", "failure", "always"] {
    assert!(!path(tag).exists(), "the {tag} step ran on the host");
  }
  Ok(())
}
