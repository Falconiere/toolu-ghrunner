//! Issue #79 expression replay through the real execution engine.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use execution::Runner;
use shared::{AgentJobRequestMessage, Conclusion, RunnerConfig, RunnerEvent, SecretMasker};
use tokio_util::sync::CancellationToken;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;
const ACQUISITION: &str = include_str!("incoming_contexts_matrix_0.json");
const ACTIONS: &str = concat!(
  env!("CARGO_MANIFEST_DIR"),
  "/../toolu-runner/tests/fixtures/local_actions"
);

async fn replay(
  action: &str,
  bad_root: bool,
) -> TestResult<(tempfile::TempDir, PathBuf, Vec<RunnerEvent>)> {
  let mut msg: AgentJobRequestMessage = serde_json::from_str(ACQUISITION)?;
  // Preserve captured UUIDs, context data, and token shapes. Substitute the
  // expression payloads and assign the companion workflow's authored action ID.
  msg.steps.retain(|step| {
    step.context_name.as_deref() == Some("__run")
      || step.context_name.as_deref() == Some("__run_2")
      || step.context_name.as_deref() == Some("__self")
      || step.context_name.as_deref() == Some("__run_3")
  });
  let probe = msg
    .steps
    .iter_mut()
    .find(|step| step.context_name.as_deref() == Some("__run_2"))
    .ok_or("captured top-level probe missing")?;
  probe.condition = Some("success() && case(true, format('{0}', -1) == '-1', false)".to_owned());
  let probe_env = probe
    .environment
    .as_mut()
    .ok_or("captured environment missing")?
    .d
    .as_mut()
    .and_then(|items| items.first_mut())
    .ok_or("captured top-level env token missing")?;
  probe_env.value.expr = Some(
    if bad_root {
      "unknown.root"
    } else {
      "case(true, format('{0}', -1), 'bad')"
    }
    .to_owned(),
  );
  let probe_script = probe
    .inputs
    .d
    .as_mut()
    .and_then(|items| items.first_mut())
    .ok_or("captured top-level script token missing")?;
  probe_script.value.expr = Some(
    "format('test \"$MATRIX\" = ''-1''\nprintf ''top-level'' > ../expression-79.top\n')".to_owned(),
  );
  let action_step = msg
    .steps
    .iter_mut()
    .find(|step| step.context_name.as_deref() == Some("__self"))
    .ok_or("captured local action missing")?;
  action_step.reference.path = Some(format!("./.github/actions/{action}"));
  // Generated __self is deliberately unavailable in steps; the live probe
  // authors id: parity so its outputs can be consumed by the following step.
  action_step.context_name = Some("parity".to_owned());
  let with_who = action_step
    .inputs
    .d
    .as_mut()
    .and_then(|items| items.first_mut())
    .ok_or("captured with.who token missing")?;
  with_who.value.expr = Some("case(true, inputs.who, 'bad')".to_owned());
  let script_step = msg
    .steps
    .iter_mut()
    .find(|step| step.context_name.as_deref() == Some("__run_3"))
    .ok_or("captured final script missing")?;
  let root_env = script_step
    .environment
    .as_mut()
    .ok_or("captured environment missing")?
    .d
    .as_mut()
    .and_then(|items| items.first_mut())
    .ok_or("captured root env token missing")?;
  root_env.value.expr = Some("steps.parity.outputs.result".to_owned());
  let script = script_step
    .inputs
    .d
    .as_mut()
    .and_then(|items| items.first_mut())
    .ok_or("captured script token missing")?;
  script.value.lit = Some("test \"$WHO\" = world/world:42/\ntest \"$(cat expression-79.composite)\" = world/world:42\nprintf '%s' \"$WHO\" > expression-79.root\n".to_owned());
  let temp = tempfile::tempdir()?;
  let config = RunnerConfig {
    data_dir: temp.path().join("data"),
    workspace_root: temp.path().join("work"),
    workspace_gc_hours: 0,
    ..RunnerConfig::default()
  };
  let workspace = config.workspace_root.join(&msg.job_id);
  for name in [action, "expression-79-child"] {
    let destination = workspace.join(".github/actions").join(name);
    std::fs::create_dir_all(&destination)?;
    std::fs::copy(
      Path::new(ACTIONS).join(name).join("action.yml"),
      destination.join("action.yml"),
    )?;
  }
  let runner = Runner::new(config, Arc::new(Mutex::new(SecretMasker::new())));
  let cancel = CancellationToken::new();
  let events = runner.execute_job(msg, cancel.clone());
  let events = tokio::time::timeout(Duration::from_secs(60), collect(events)).await?;
  cancel.cancel();
  Ok((temp, workspace, events))
}

async fn collect(mut receiver: tokio::sync::mpsc::Receiver<RunnerEvent>) -> Vec<RunnerEvent> {
  let mut events = Vec::new();
  while let Some(event) = receiver.recv().await {
    events.push(event);
  }
  events
}

fn conclusion(events: &[RunnerEvent]) -> Option<Conclusion> {
  events.iter().find_map(|event| {
    if let RunnerEvent::JobCompleted { conclusion, .. } = event {
      Some(*conclusion)
    } else {
      None
    }
  })
}

#[tokio::test]
async fn captured_job_replays_expression_fields_through_composite_and_root_script() -> TestResult {
  let (_temp, workspace, events) = replay("expression-79", false).await?;
  assert_eq!(conclusion(&events), Some(Conclusion::Success), "{events:?}");
  assert_eq!(
    std::fs::read_to_string(workspace.join("expression-79.composite"))?,
    "world/world:42"
  );
  assert_eq!(
    std::fs::read_to_string(workspace.join("expression-79.top"))?,
    "top-level"
  );
  assert_eq!(
    std::fs::read_to_string(workspace.join("expression-79.root"))?,
    "world/world:42/"
  );
  assert!(events.iter().any(|event| matches!(event, RunnerEvent::StepCompleted { outputs, .. } if outputs.get("result").is_some_and(|value| value == "world/world:42/"))));
  Ok(())
}

#[tokio::test]
async fn bad_root_expression_is_visible_and_fails_the_captured_environment() -> TestResult {
  let (_temp, _workspace, events) = replay("expression-79", true).await?;
  assert_eq!(conclusion(&events), Some(Conclusion::Failure));
  assert!(
    events
      .iter()
      .any(|event| matches!(event, RunnerEvent::Log { line, .. } if line.starts_with("##[error]") && line.contains("unknown named value: unknown"))),
    "{events:?}"
  );
  Ok(())
}

#[tokio::test]
async fn bad_composite_root_is_visible_and_fails() -> TestResult {
  let (_temp, _workspace, events) = replay("expression-79-bad", false).await?;
  assert_eq!(conclusion(&events), Some(Conclusion::Failure));
  assert!(events.iter().any(|event| matches!(event, RunnerEvent::Log { line, .. } if line.contains("unavailable in composite metadata"))), "{events:?}");
  Ok(())
}
