//! Issue #102 composite expression and conditional cleanup regressions.
//! The source job is a sanitized real #68 acquisition; only its local action
//! path is changed to the committed probe action. UUID/contextName and the
//! expression-valued `with.who` token stay in the captured wire shape.

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

async fn replay(action: &str) -> TestResult<(tempfile::TempDir, PathBuf, Vec<RunnerEvent>)> {
  replay_with_options(action, false, None).await
}

async fn replay_with_cancel(
  action: &str,
  cancel_after_start: bool,
) -> TestResult<(tempfile::TempDir, PathBuf, Vec<RunnerEvent>)> {
  replay_with_options(action, cancel_after_start, None).await
}

async fn replay_with_options(
  action: &str,
  cancel_after_start: bool,
  outer_script: Option<&str>,
) -> TestResult<(tempfile::TempDir, PathBuf, Vec<RunnerEvent>)> {
  let mut msg: AgentJobRequestMessage = serde_json::from_str(ACQUISITION)?;
  msg.steps.retain(|step| {
    step.reference.path.as_deref() == Some("./.github/actions/context-68-parent")
      && step.context_name.as_deref() == Some("__self")
      || outer_script.is_some() && step.context_name.as_deref() == Some("__run_3")
  });
  let step = msg
    .steps
    .first_mut()
    .ok_or("captured local action step missing")?;
  step.reference.path = Some(format!("./.github/actions/{action}"));
  if let Some(outer_script) = outer_script {
    let outer = msg
      .steps
      .iter_mut()
      .find(|step| step.context_name.as_deref() == Some("__run_3"))
      .ok_or("captured outer run step missing")?;
    let script = outer
      .inputs
      .d
      .as_mut()
      .and_then(|entries| entries.first_mut())
      .ok_or("captured outer script token missing")?;
    script.value.token_type = 0;
    script.value.expr = None;
    script.value.lit = Some(outer_script.to_owned());
  }

  let temp = tempfile::tempdir()?;
  let config = RunnerConfig {
    data_dir: temp.path().join("data"),
    workspace_root: temp.path().join("work"),
    workspace_gc_hours: 0,
    ..RunnerConfig::default()
  };
  let workspace = config.workspace_root.join(&msg.job_id);
  let dest = workspace.join(".github/actions").join(action);
  std::fs::create_dir_all(&dest)?;
  std::fs::copy(
    Path::new(ACTIONS).join(action).join("action.yml"),
    dest.join("action.yml"),
  )?;
  if action == "composite-102-parent" {
    let nested = workspace.join(".github/actions/composite-102-nested");
    std::fs::create_dir_all(&nested)?;
    std::fs::copy(
      Path::new(ACTIONS).join("composite-102-nested/action.yml"),
      nested.join("action.yml"),
    )?;
  }
  if action == "composite-102-file-commands" || action == "composite-102-post-status" {
    let node_action = if action == "composite-102-post-status" {
      "node-102-post-failure"
    } else {
      "node-102-state"
    };
    let source = Path::new(ACTIONS).join(node_action);
    let nested = workspace.join(".github/actions").join(node_action);
    std::fs::create_dir_all(&nested)?;
    for file in ["action.yml", "main.js", "post.js"] {
      std::fs::copy(source.join(file), nested.join(file))?;
    }
    if action == "composite-102-file-commands" {
      std::fs::copy(source.join("pre.js"), nested.join("pre.js"))?;
    }
  }

  let runner = Runner::new(config, Arc::new(Mutex::new(SecretMasker::new())));
  let cancel = CancellationToken::new();
  let events = runner.execute_job(msg, cancel.clone());
  if cancel_after_start {
    let started = workspace.join("composite-102-bounds.started");
    tokio::time::timeout(Duration::from_secs(5), async {
      while !started.exists() {
        tokio::time::sleep(Duration::from_millis(10)).await;
      }
    })
    .await?;
    cancel.cancel();
  }
  let collected = tokio::time::timeout(Duration::from_secs(240), collect(events)).await?;
  cancel.cancel();
  Ok((temp, workspace, collected))
}

async fn collect(mut receiver: tokio::sync::mpsc::Receiver<RunnerEvent>) -> Vec<RunnerEvent> {
  let mut events = Vec::new();
  while let Some(event) = receiver.recv().await {
    events.push(event);
  }
  events
}

fn job_conclusion(events: &[RunnerEvent]) -> Option<Conclusion> {
  events.iter().find_map(|event| {
    if let RunnerEvent::JobCompleted { conclusion, .. } = event {
      Some(*conclusion)
    } else {
      None
    }
  })
}

#[tokio::test]
async fn expressions_render_functions_brackets_conditions_and_outputs() -> TestResult {
  let (_temp, workspace, events) = replay("composite-102-expressions").await?;
  assert_eq!(
    job_conclusion(&events),
    Some(Conclusion::Success),
    "{events:?}"
  );
  assert_eq!(
    std::fs::read_to_string(workspace.join("composite-102-expressions.out"))?,
    "hello world"
  );
  let output = events.iter().find_map(|event| {
    if let RunnerEvent::StepCompleted { outputs, .. } = event {
      outputs.get("result").cloned()
    } else {
      None
    }
  });
  assert_eq!(output.as_deref(), Some("world/42"));
  Ok(())
}

#[tokio::test]
async fn conditions_run_failure_and_always_cleanup_after_inner_exit_one() -> TestResult {
  let (_temp, workspace, events) = replay("composite-102-failure").await?;
  assert_eq!(
    job_conclusion(&events),
    Some(Conclusion::Failure),
    "{events:?}"
  );
  assert_eq!(
    std::fs::read_to_string(workspace.join("composite-102-failure.marker"))?
      .lines()
      .collect::<Vec<_>>(),
    vec!["fail", "failure-cleanup", "always-cleanup"]
  );
  let parent_id = "f8bcd552-f40d-4cdc-94de-8f9fbf4dd191";
  let inner_log = events.iter().position(|event| matches!(event, RunnerEvent::Log { step_id, line, .. } if step_id == parent_id && line == "INNER_LOG=fail"));
  let completion = events.iter().position(
    |event| matches!(event, RunnerEvent::StepCompleted { step_id, .. } if step_id == parent_id),
  );
  assert!(
    inner_log
      .zip(completion)
      .is_some_and(|(log, done)| log < done),
    "{events:?}"
  );
  Ok(())
}

#[tokio::test]
async fn continue_on_error_keeps_raw_failure_and_runs_ordinary_and_always() -> TestResult {
  let (_temp, workspace, events) = replay("composite-102-continue").await?;
  assert_eq!(
    job_conclusion(&events),
    Some(Conclusion::Success),
    "{events:?}"
  );
  assert_eq!(
    std::fs::read_to_string(workspace.join("composite-102-continue.marker"))?
      .lines()
      .collect::<Vec<_>>(),
    vec!["fail", "ordinary", "always-cleanup"]
  );
  assert_eq!(
    std::fs::read_to_string(workspace.join("composite-102-continue.result"))?,
    "failure/success"
  );
  Ok(())
}

#[tokio::test]
async fn hard_nested_error_keeps_parent_attribution_and_runs_failure_cleanup() -> TestResult {
  let (_temp, workspace, events) = replay("composite-102-hard-error").await?;
  assert_eq!(
    job_conclusion(&events),
    Some(Conclusion::Failure),
    "{events:?}"
  );
  assert_eq!(
    std::fs::read_to_string(workspace.join("composite-102-hard-error.marker"))?
      .lines()
      .collect::<Vec<_>>(),
    vec!["failure-cleanup", "always-cleanup"]
  );
  assert!(events.iter().any(|event| matches!(
    event,
    RunnerEvent::Log { step_id, line, .. }
      if step_id == "f8bcd552-f40d-4cdc-94de-8f9fbf4dd191" && line.starts_with("##[error]")
  )));
  Ok(())
}

#[tokio::test]
async fn nested_repeated_names_keep_distinct_inputs_and_outputs() -> TestResult {
  let (_temp, workspace, events) = replay("composite-102-parent").await?;
  assert_eq!(
    job_conclusion(&events),
    Some(Conclusion::Success),
    "{events:?}"
  );
  assert_eq!(
    std::fs::read_to_string(workspace.join("composite-102-parent.out"))?,
    "world-one/world-two"
  );
  assert_eq!(
    std::fs::read_to_string(workspace.join("composite-102-parent.scope"))?,
    "world/success/success/"
  );
  let output = events.iter().find_map(|event| {
    if let RunnerEvent::StepCompleted { outputs, .. } = event {
      outputs.get("result").cloned()
    } else {
      None
    }
  });
  assert_eq!(output.as_deref(), Some("world-one/world-two"));
  Ok(())
}

#[tokio::test]
async fn file_commands_step_env_and_nested_posts_are_scoped() -> TestResult {
  let (_temp, workspace, events) =
    replay_with_options("composite-102-file-commands", false, Some("test \"$COMPOSITE_VALUE\" = inner\ntest -z \"${LOCAL_ONLY:-}\"\ncommand -v composite-102-tool\necho outer-ok > composite-102-file-commands.outer")).await?;
  assert_eq!(
    job_conclusion(&events),
    Some(Conclusion::Success),
    "{events:?}"
  );
  assert_eq!(
    std::fs::read_to_string(workspace.join("composite-102-file-commands.out"))?,
    "inner/absent/42"
  );
  assert_eq!(
    std::fs::read_to_string(workspace.join("composite-102-file-commands.outer"))?,
    "outer-ok\n"
  );
  let post_lines: Vec<_> = events
    .iter()
    .filter_map(|event| {
      if let RunnerEvent::Log { step_id, line, .. } = event
        && line.starts_with("STATE_POST=")
      {
        Some((step_id.as_str(), line.as_str()))
      } else {
        None
      }
    })
    .collect();
  assert_eq!(
    post_lines.iter().map(|(_, line)| *line).collect::<Vec<_>>(),
    ["STATE_POST=two", "STATE_POST=one"]
  );
  assert_ne!(
    post_lines.first().map(|line| line.0),
    post_lines.get(1).map(|line| line.0)
  );
  for (post_id, _) in post_lines {
    assert!(events.iter().any(|event| matches!(event, RunnerEvent::StepCompleted { step_id, conclusion: Conclusion::Success, .. } if step_id == post_id)));
  }
  assert!(events.iter().any(|event| matches!(event, RunnerEvent::StepCompleted { outputs, .. } if outputs.get("result").is_some_and(|value| value == "42"))));
  Ok(())
}

#[tokio::test]
async fn observed_start_cancel_kills_child_and_runs_always_cleanup() -> TestResult {
  let (_temp, workspace, events) = replay_with_cancel("composite-102-bounds", true).await?;
  assert_eq!(
    job_conclusion(&events),
    Some(Conclusion::Cancelled),
    "{events:?}"
  );
  assert!(
    events.iter().any(|event| matches!(event,
      RunnerEvent::StepCompleted { step_id, conclusion: Conclusion::Cancelled, .. }
        if step_id == "f8bcd552-f40d-4cdc-94de-8f9fbf4dd191"
    )),
    "{events:?}"
  );
  assert!(workspace.join("composite-102-bounds.started").exists());
  assert!(workspace.join("composite-102-bounds.cleanup").exists());
  assert!(!workspace.join("composite-102-bounds.ordinary").exists());
  assert!(!workspace.join("composite-102-bounds.late").exists());
  for file in ["composite-102-bounds.pid", "composite-102-bounds.sleep-pid"] {
    let pid = std::fs::read_to_string(workspace.join(file))?;
    let output = std::process::Command::new("ps")
      .args(["-p", pid.trim(), "-o", "stat="])
      .output()?;
    let state = String::from_utf8(output.stdout)?;
    assert!(
      !output.status.success() || state.trim().starts_with('Z'),
      "cancelled process still alive: {pid} {state}"
    );
  }
  Ok(())
}

fn has_parent_expression_error(events: &[RunnerEvent]) -> bool {
  events.iter().any(|event| {
    matches!(
      event,
      RunnerEvent::Log { step_id, line, .. }
        if step_id == "f8bcd552-f40d-4cdc-94de-8f9fbf4dd191" && line.starts_with("##[error]")
    )
  })
}

#[tokio::test]
async fn malformed_run_expression_fails_step_then_runs_cleanup() -> TestResult {
  let (_temp, workspace, events) = replay("composite-102-malformed").await?;
  assert_eq!(job_conclusion(&events), Some(Conclusion::Failure));
  assert!(has_parent_expression_error(&events));
  assert_eq!(
    std::fs::read_to_string(workspace.join("composite-102-malformed.marker"))?
      .lines()
      .collect::<Vec<_>>(),
    ["failure-cleanup", "always-cleanup"]
  );
  Ok(())
}

#[tokio::test]
async fn forbidden_context_fails_visibly_then_runs_cleanup() -> TestResult {
  let (_temp, workspace, events) = replay("composite-102-forbidden").await?;
  assert_eq!(job_conclusion(&events), Some(Conclusion::Failure));
  assert!(has_parent_expression_error(&events));
  assert_eq!(
    std::fs::read_to_string(workspace.join("composite-102-forbidden.marker"))?,
    "cleaned\n"
  );
  Ok(())
}

#[tokio::test]
async fn malformed_if_expression_stops_composite_loop() -> TestResult {
  let (_temp, workspace, events) = replay("composite-102-bad-if").await?;
  assert_eq!(job_conclusion(&events), Some(Conclusion::Failure));
  assert!(has_parent_expression_error(&events));
  assert!(!workspace.join("composite-102-bad-if.marker").exists());
  Ok(())
}

#[tokio::test]
async fn nested_post_if_failure_sees_later_global_job_failure() -> TestResult {
  let (_temp, _workspace, events) =
    replay_with_options("composite-102-post-status", false, Some("exit 1")).await?;
  assert_eq!(job_conclusion(&events), Some(Conclusion::Failure));
  assert!(
    events
      .iter()
      .any(|event| matches!(event, RunnerEvent::Log { line, .. } if line == "STATE_POST=world")),
    "{events:?}"
  );
  Ok(())
}
