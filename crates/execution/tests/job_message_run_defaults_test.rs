//! Replay GitHub's acquired run-defaults message through the production engine.
//!
//! Boundary cases modify only the named field of the sanitized acquisition.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use execution::Runner;
use execution::execution::context::ExecutionContext;
use execution::execution::job_spec::{JobSpec, RunDefaultsResolved};
use shared::{
  ActionStep, AgentJobRequestMessage, Conclusion, RunnerConfig, RunnerEvent, SecretMasker,
  TemplateToken,
};
use tokio_util::sync::CancellationToken;

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;
const CAPTURE: &str = include_str!("defaults_run_job.json");

fn captured(indices: &[usize]) -> TestResult<AgentJobRequestMessage> {
  let mut message: AgentJobRequestMessage = serde_json::from_str(CAPTURE)?;
  // Checkout is acquisition setup. Keep real step IDs, scripts and inputs.
  message.steps = indices
    .iter()
    .map(|index| {
      message
        .steps
        .get(*index)
        .cloned()
        .ok_or("captured step missing".into())
    })
    .collect::<TestResult<Vec<_>>>()?;
  Ok(message)
}

fn set_step_input(step: &mut ActionStep, name: &str, value: &str) -> TestResult {
  let entry = step
    .inputs
    .d
    .as_mut()
    .ok_or("step inputs missing")?
    .iter_mut()
    .find(|entry| entry.key.to_string_value() == Some(name))
    .ok_or_else(|| format!("{name} input missing"))?;
  entry.value.lit = Some(value.to_owned());
  entry.value.token_type = 0;
  Ok(())
}

fn job_run_value<'a>(
  message: &'a mut AgentJobRequestMessage,
  name: &str,
) -> TestResult<&'a mut TemplateToken> {
  let layer = message.defaults.get_mut(1).ok_or("job defaults missing")?;
  let run = layer
    .d
    .as_mut()
    .ok_or("layer entries missing")?
    .first_mut()
    .ok_or("run entry missing")?;
  run
    .value
    .d
    .as_mut()
    .ok_or("run entries missing")?
    .iter_mut()
    .find(|entry| entry.key.to_string_value() == Some(name))
    .map(|entry| &mut entry.value)
    .ok_or_else(|| format!("{name} default missing").into())
}

async fn replay(
  mut message: AgentJobRequestMessage,
  copy_composite: bool,
) -> TestResult<(tempfile::TempDir, PathBuf, Vec<RunnerEvent>)> {
  // macOS reports /private/var as PWD for a /var temp directory. The captured
  // absolute-path assertion compared those aliases byte-for-byte; compare
  // their physical paths while preserving the acquired step's cwd and ID.
  if let Some(step) = message
    .steps
    .iter_mut()
    .find(|step| step.context_name.as_deref() == Some("__run_3"))
  {
    set_step_input(
      step,
      "script",
      "test \"$(pwd -P)\" = \"$(cd \"$RUNNER_TEMP/defaults-71-absolute\" && pwd -P)\"; printf absolute > absolute.marker",
    )?;
  }
  let dir = tempfile::tempdir()?;
  let config = RunnerConfig {
    data_dir: dir.path().join("data"),
    workspace_root: dir.path().join("work"),
    workspace_gc_hours: 0,
    ..RunnerConfig::default()
  };
  let workspace = config.workspace_root.join(&message.job_id);
  if copy_composite {
    let target = workspace.join(".github/actions/defaults-71-composite");
    std::fs::create_dir_all(&target)?;
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
      .join("../../.github/actions/defaults-71-composite/action.yml");
    std::fs::copy(source, target.join("action.yml"))?;
  }
  let runner = Runner::new(config, Arc::new(Mutex::new(SecretMasker::new())));
  let mut stream = runner.execute_job(message, CancellationToken::new());
  let events = tokio::time::timeout(Duration::from_secs(60), async {
    let mut events = Vec::new();
    while let Some(event) = stream.recv().await {
      events.push(event);
    }
    events
  })
  .await?;
  Ok((dir, workspace, events))
}

fn assert_success(events: &[RunnerEvent], step_ids: &[String]) {
  let completed: Vec<(&str, Conclusion)> = events
    .iter()
    .filter_map(|event| match event {
      RunnerEvent::StepCompleted {
        step_id,
        conclusion,
        ..
      } => Some((step_id.as_str(), *conclusion)),
      RunnerEvent::JobStarted { .. }
      | RunnerEvent::StepStarted { .. }
      | RunnerEvent::StepSkipped { .. }
      | RunnerEvent::Log { .. }
      | RunnerEvent::LogGroup { .. }
      | RunnerEvent::Annotation { .. }
      | RunnerEvent::JobCompleted { .. } => None,
    })
    .collect();
  let expected: Vec<(&str, Conclusion)> = step_ids
    .iter()
    .map(|id| (id.as_str(), Conclusion::Success))
    .collect();
  assert_eq!(completed, expected, "{events:?}");
  assert!(
    events.iter().any(|event| matches!(
      event,
      RunnerEvent::JobCompleted {
        conclusion: Conclusion::Success,
        ..
      }
    )),
    "{events:?}"
  );
}

#[tokio::test]
async fn job_defaults_override_workflow() -> TestResult {
  let message = captured(&[1, 2])?;
  let ids = message
    .steps
    .iter()
    .map(|step| step.id.clone())
    .collect::<Vec<_>>();
  let (_dir, workspace, events) = replay(message, false).await?;
  assert_success(&events, &ids);
  assert_eq!(
    std::fs::read_to_string(workspace.join("defaults-71-job/job.marker"))?,
    "job"
  );
  assert!(!workspace.join("defaults-71-workflow/job.marker").exists());
  Ok(())
}

#[tokio::test]
async fn step_values_override_defaults() -> TestResult {
  let message = captured(&[1, 3])?;
  let ids = message
    .steps
    .iter()
    .map(|step| step.id.clone())
    .collect::<Vec<_>>();
  let (_dir, workspace, events) = replay(message, false).await?;
  assert_success(&events, &ids);
  assert_eq!(
    std::fs::read_to_string(workspace.join("defaults-71-step/explicit.marker"))?,
    "explicit"
  );
  assert!(!workspace.join("defaults-71-job/explicit.marker").exists());
  Ok(())
}

#[tokio::test]
async fn paths_and_absence() -> TestResult {
  let message = captured(&[1, 4, 5])?;
  let ids = message
    .steps
    .iter()
    .map(|step| step.id.clone())
    .collect::<Vec<_>>();
  let (_dir, workspace, events) = replay(message, false).await?;
  assert_success(&events, &ids);
  assert_eq!(
    std::fs::read_to_string(workspace.join("defaults-71 with spaces/spaces.marker"))?,
    "spaces"
  );
  let mut absent = captured(&[2])?;
  absent.defaults.clear();
  let step = absent.steps.first_mut().ok_or("step missing")?;
  set_step_input(step, "script", "printf root > root.marker")?;
  let (_dir, workspace, events) = replay(absent, false).await?;
  assert!(
    events.iter().any(|event| matches!(
      event,
      RunnerEvent::JobCompleted {
        conclusion: Conclusion::Success,
        ..
      }
    )),
    "{events:?}"
  );
  assert_eq!(
    std::fs::read_to_string(workspace.join("root.marker"))?,
    "root"
  );
  Ok(())
}

#[tokio::test]
async fn empty_explicit_shell_uses_job_shell() -> TestResult {
  let mut message = captured(&[1, 3])?;
  let step = message.steps.get_mut(1).ok_or("explicit step missing")?;
  set_step_input(step, "shell", "")?;
  set_step_input(
    step,
    "script",
    "test \"$(ps -p $$ -o comm= | tr -d '[:space:]')\" = sh; printf empty > empty.marker",
  )?;
  let ids = message
    .steps
    .iter()
    .map(|step| step.id.clone())
    .collect::<Vec<_>>();
  let (_dir, workspace, events) = replay(message, false).await?;
  assert_success(&events, &ids);
  assert_eq!(
    std::fs::read_to_string(workspace.join("defaults-71-step/empty.marker"))?,
    "empty"
  );
  Ok(())
}

#[tokio::test]
async fn nonexistent_default_directory_fails_its_step_with_path() -> TestResult {
  let mut message = captured(&[2])?;
  job_run_value(&mut message, "working-directory")?.lit = Some("missing-71-cwd".to_owned());
  let step_id = message.steps.first().ok_or("step missing")?.id.clone();
  let (_dir, workspace, events) = replay(message, false).await?;
  assert!(events.iter().any(|event| matches!(
    event,
    RunnerEvent::StepCompleted { step_id: id, conclusion: Conclusion::Failure, .. } if id == &step_id
  )));
  let expected = workspace.join("missing-71-cwd").display().to_string();
  assert!(
    events.iter().any(|event| matches!(
      event,
      RunnerEvent::Log { step_id: id, line, .. } if id == &step_id && line.contains(&expected)
    )),
    "{events:?}"
  );
  Ok(())
}

#[tokio::test]
async fn composite_scope() -> TestResult {
  let message = captured(&[1, 2, 3, 4, 5, 6, 7])?;
  let ids = message
    .steps
    .iter()
    .map(|step| step.id.clone())
    .collect::<Vec<_>>();
  let (_dir, workspace, events) = replay(message, true).await?;
  assert_success(&events, &ids);
  assert_eq!(
    std::fs::read_to_string(workspace.join(".defaults-71-composite.marker"))?,
    "composite"
  );
  assert_eq!(
    std::fs::read_to_string(workspace.join("defaults-71-job/job.marker"))?,
    "job"
  );
  assert!(
    !workspace
      .join("defaults-71-job/.defaults-71-composite.marker")
      .exists()
  );
  Ok(())
}

fn parsed_defaults(defaults: &[TemplateToken]) -> TestResult<RunDefaultsResolved> {
  let ctx = ExecutionContext::with_masker(Arc::new(Mutex::new(SecretMasker::new())));
  Ok(JobSpec::from_message_defaults(defaults, &ctx)?)
}

#[test]
fn later_partial_mapping_clears_earlier_shell() -> TestResult {
  let mut message = captured(&[])?;
  let layer = message.defaults.get_mut(1).ok_or("job defaults missing")?;
  let run = layer
    .d
    .as_mut()
    .ok_or("layer entries missing")?
    .first_mut()
    .ok_or("run entry missing")?;
  run
    .value
    .d
    .as_mut()
    .ok_or("run entries missing")?
    .retain(|entry| entry.key.to_string_value() != Some("shell"));
  let resolved = parsed_defaults(&message.defaults)?;
  assert_eq!(resolved.shell, None);
  assert_eq!(
    resolved.working_directory.as_deref(),
    Some("defaults-71-job")
  );
  Ok(())
}

#[test]
fn empty_job_value_clears_earlier_workflow_value() -> TestResult {
  let mut message = captured(&[])?;
  job_run_value(&mut message, "shell")?.lit = Some(String::new());
  let resolved = parsed_defaults(&message.defaults)?;
  assert_eq!(resolved.shell, None);
  assert_eq!(
    resolved.working_directory.as_deref(),
    Some("defaults-71-job")
  );
  Ok(())
}

#[test]
fn keys_are_ascii_case_insensitive() -> TestResult {
  let mut message = captured(&[])?;
  let layer = message.defaults.get_mut(1).ok_or("job defaults missing")?;
  let run = layer
    .d
    .as_mut()
    .ok_or("layer entries missing")?
    .first_mut()
    .ok_or("run entry missing")?;
  run.key.lit = Some("RUN".to_owned());
  for entry in run.value.d.as_mut().ok_or("run entries missing")? {
    entry.key.lit = entry.key.lit.as_ref().map(|name| name.to_ascii_uppercase());
  }
  let resolved = parsed_defaults(&message.defaults)?;
  assert_eq!(resolved.shell.as_deref(), Some("sh"));
  assert_eq!(
    resolved.working_directory.as_deref(),
    Some("defaults-71-job")
  );
  Ok(())
}

#[test]
fn failed_default_expression_is_an_error() -> TestResult {
  let mut message = captured(&[])?;
  let shell = job_run_value(&mut message, "shell")?;
  shell.token_type = 3;
  shell.expr = Some("unknownFunction()".to_owned());
  let error = parsed_defaults(&message.defaults)
    .err()
    .ok_or("bad expression accepted")?;
  assert!(error.to_string().contains("expression"), "{error}");
  Ok(())
}

#[test]
fn malformed_defaults() -> TestResult {
  let mut message = captured(&[])?;
  message
    .defaults
    .get_mut(1)
    .ok_or("job defaults missing")?
    .token_type = 1;
  let error = parsed_defaults(&message.defaults)
    .err()
    .ok_or("malformed layer accepted")?;
  assert!(
    error
      .to_string()
      .contains("defaults must be a mapping token")
  );
  Ok(())
}
