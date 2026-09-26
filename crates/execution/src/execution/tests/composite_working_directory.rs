//! #81 real Bash probes through the captured #68 acquisition and production Runner.
//! Keep UUID/contextName and input expression tokens; replace the selected local
//! action and final script. Caller defaults come from the real #71 capture.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use shared::{AgentJobRequestMessage, Conclusion, RunnerConfig, RunnerEvent, SecretMasker};
use tokio_util::sync::CancellationToken;

use crate::Runner;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;
const ACQUISITION: &str = include_str!("../../../tests/incoming_contexts_matrix_0.json");
const DEFAULTS: &str = include_str!("../../../tests/defaults_run_job.json");
const ACTIONS: &str = concat!(
  env!("CARGO_MANIFEST_DIR"),
  "/../toolu-runner/tests/fixtures/local_actions"
);

struct Replay {
  _temp: tempfile::TempDir,
  workspace: PathBuf,
  runner_temp: PathBuf,
  events: Vec<RunnerEvent>,
}

async fn replay(action: &str, caller_defaults: bool, not_directory: bool) -> TestResult<Replay> {
  let mut msg: AgentJobRequestMessage = serde_json::from_str(ACQUISITION)?;
  msg.steps.retain(|step| {
    step.context_name.as_deref() == Some("__self")
      || caller_defaults && step.context_name.as_deref() == Some("__run_3")
  });
  msg
    .steps
    .first_mut()
    .ok_or("missing action")?
    .reference
    .path = Some(format!("./.github/actions/{action}"));
  if caller_defaults {
    let captured: AgentJobRequestMessage = serde_json::from_str(DEFAULTS)?;
    msg.defaults = captured.defaults;
    // Keep the captured map token shape; select a dedicated directory for this probe.
    for layer in &mut msg.defaults {
      for entry in layer.d.iter_mut().flatten() {
        if entry.key.to_string_value() == Some("run") {
          for field in entry.value.d.iter_mut().flatten() {
            if field.key.to_string_value() == Some("working-directory") {
              field.value.token_type = 0;
              field.value.expr = None;
              field.value.lit = Some("caller-default".to_owned());
            }
          }
        }
      }
    }
    let outer = msg.steps.last_mut().ok_or("missing caller step")?;
    let script = outer
      .inputs
      .d
      .as_mut()
      .and_then(|entries| {
        entries
          .iter_mut()
          .find(|e| e.key.to_string_value() == Some("script"))
      })
      .ok_or("missing caller script")?;
    script.value.token_type = 0;
    script.value.expr = None;
    script.value.lit = Some("pwd -P > caller.pwd".to_owned());
  }
  let temp = tempfile::Builder::new()
    .prefix("composite cwd ")
    .tempdir()?;
  let config = RunnerConfig {
    data_dir: temp.path().join("data"),
    workspace_root: temp.path().join("work"),
    workspace_gc_hours: 0,
    ..RunnerConfig::default()
  };
  let workspace = config.workspace_root.join(&msg.job_id);
  std::fs::create_dir_all(workspace.join("caller-default"))?;
  for name in [action, "composite-81-nested"] {
    let dest = workspace.join(".github/actions").join(name);
    std::fs::create_dir_all(&dest)?;
    std::fs::copy(
      Path::new(ACTIONS).join(name).join("action.yml"),
      dest.join("action.yml"),
    )?;
  }
  if not_directory {
    std::fs::write(workspace.join("missing-directory-81"), "regular file")?;
  }
  let runner_temp = config.data_dir.join("_temp");
  let runner = Runner::new(config, Arc::new(Mutex::new(SecretMasker::new())));
  let mut receiver = runner.execute_job(msg, CancellationToken::new());
  let events = tokio::time::timeout(Duration::from_secs(30), async {
    let mut events = Vec::new();
    while let Some(event) = receiver.recv().await {
      events.push(event);
    }
    events
  })
  .await?;
  Ok(Replay {
    _temp: temp,
    workspace,
    runner_temp,
    events,
  })
}

fn assert_conclusion(replay: &Replay, expected: Conclusion) {
  assert!(
    replay.events.iter().any(|event| matches!(
      event, RunnerEvent::JobCompleted { conclusion, .. } if *conclusion == expected
    )),
    "{:?}",
    replay.events
  );
}

fn assert_pwd(directory: &Path, name: &str) -> TestResult {
  let actual = std::fs::read_to_string(directory.join(name))?;
  assert_eq!(Path::new(actual.trim_end()), directory.canonicalize()?);
  Ok(())
}

fn assert_output(replay: &Replay, expected: &str) {
  assert!(
    replay.events.iter().any(|event| matches!(
      event, RunnerEvent::StepCompleted { outputs, .. }
        if outputs.get("result").is_some_and(|value| value == expected)
    )),
    "{:?}",
    replay.events
  );
}

#[tokio::test]
async fn literal_expression_absolute_spaces_and_empty_directories() -> TestResult {
  let replay = replay("composite-81-paths", false, false).await?;
  assert_conclusion(&replay, Conclusion::Success);
  for (directory, file) in [
    ("literal", "literal.pwd"),
    ("world", "input.pwd"),
    ("space dir", "spaces.pwd"),
    ("space dir", "local-env.pwd"),
    ("output-dir", "output.pwd"),
    ("env-dir", "env.pwd"),
    ("", "empty.pwd"),
    ("", "missing-input.pwd"),
    ("", "absent.pwd"),
    ("", "parent.pwd"),
  ] {
    assert_pwd(&replay.workspace.join(directory), file)?;
  }
  assert_pwd(&replay.runner_temp, "absolute.pwd")?;
  assert!(!replay.workspace.join("literal.pwd").exists());
  assert!(!replay.workspace.join("absolute.pwd").exists());
  Ok(())
}

#[tokio::test]
async fn nested_directories_ignore_caller_defaults_and_restore_scope() -> TestResult {
  let replay = replay("composite-81-parent", true, false).await?;
  assert_conclusion(&replay, Conclusion::Success);
  for (directory, file) in [
    ("outer", "outer.pwd"),
    ("inner", "nested.pwd"),
    ("second", "nested.pwd"),
    ("", "nested-root.pwd"),
    ("", "parent-restored.pwd"),
    (".github/actions/composite-81-parent", "action.pwd"),
    (".github/actions/composite-81-nested", "action.pwd"),
    ("caller-default", "caller.pwd"),
  ] {
    assert_pwd(&replay.workspace.join(directory), file)?;
  }
  assert!(
    !replay
      .workspace
      .join("caller-default/nested-root.pwd")
      .exists()
  );
  Ok(())
}

async fn assert_directory_failure(action: &str, not_directory: bool) -> TestResult {
  let replay = replay(action, false, not_directory).await?;
  assert_conclusion(&replay, Conclusion::Failure);
  assert_output(&replay, "failure/failure/skipped");
  assert_eq!(
    std::fs::read_to_string(replay.workspace.join("cleanup.marker"))?,
    "failure\nalways\n"
  );
  assert!(!replay.workspace.join("unexpected.marker").exists());
  assert!(!replay.workspace.join("unexpected-success.marker").exists());
  assert!(
    replay.events.iter().any(|event| matches!(
      event, RunnerEvent::Log { step_id, line, .. }
        if step_id == "f8bcd552-f40d-4cdc-94de-8f9fbf4dd191" && line.starts_with("##[error]")
    )),
    "{:?}",
    replay.events
  );
  Ok(())
}

#[tokio::test]
async fn missing_directory_fails_inner_step_and_runs_cleanup() -> TestResult {
  assert_directory_failure("composite-81-failure", false).await
}

#[tokio::test]
async fn regular_file_directory_fails_inner_step_and_runs_cleanup() -> TestResult {
  assert_directory_failure("composite-81-failure", true).await
}

#[tokio::test]
async fn malformed_directory_expression_fails_inner_step_and_runs_cleanup() -> TestResult {
  assert_directory_failure("composite-81-malformed", false).await
}

#[tokio::test]
async fn continued_directory_failure_keeps_outcome_and_runs_successors() -> TestResult {
  let replay = replay("composite-81-continued", false, false).await?;
  assert_conclusion(&replay, Conclusion::Success);
  assert_output(&replay, "failure/success/success");
  assert_eq!(
    std::fs::read_to_string(replay.workspace.join("cleanup.marker"))?,
    "ordinary\nalways\n"
  );
  assert!(!replay.workspace.join("unexpected.marker").exists());
  Ok(())
}
