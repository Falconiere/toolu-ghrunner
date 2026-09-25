//! Captured GitHub job replay of the environment observed by real step children.

use std::error::Error;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use execution::Runner;
use shared::{
  AgentJobRequestMessage, Conclusion, DictEntry, RunnerConfig, RunnerEvent, SecretMasker,
  TemplateToken, VariableValue,
};
use tokio_util::sync::CancellationToken;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;
const CAPTURE: &str = include_str!("incoming_contexts_matrix_0.json");
const PROBE: &str = "printf 'CI72|%s|%s|%s\\n' \"${CI+x}\" \"${CI-}\" \"${GITHUB_ACTIONS-}\"";

fn literal(value: &str) -> TemplateToken {
  TemplateToken {
    token_type: 0,
    lit: Some(value.to_owned()),
    ..TemplateToken::default()
  }
}

fn mapping(key: &str, value: &str) -> TemplateToken {
  mapping_pairs(&[(key, value)])
}

fn mapping_pairs(values: &[(&str, &str)]) -> TemplateToken {
  TemplateToken {
    token_type: 2,
    d: Some(
      values
        .iter()
        .map(|(key, value)| DictEntry {
          key: literal(key),
          value: literal(value),
        })
        .collect(),
    ),
    ..TemplateToken::default()
  }
}

fn set_script(step: &mut shared::ActionStep, body: &str) -> TestResult {
  let script = step
    .inputs
    .d
    .as_mut()
    .and_then(|entries| {
      entries
        .iter_mut()
        .find(|entry| entry.key.to_string_value() == Some("script"))
    })
    .ok_or("captured script token absent")?;
  script.value = literal(body);
  Ok(())
}

fn set_false_github_actions(job: &mut AgentJobRequestMessage) {
  job.variables.insert(
    "GITHUB_ACTIONS".to_owned(),
    VariableValue {
      value: "false".to_owned(),
      is_secret: false,
    },
  );
}

async fn replay_script(job_ci: Option<&str>, step_ci: Option<&str>) -> TestResult<Vec<String>> {
  let mut job: AgentJobRequestMessage = serde_json::from_str(CAPTURE)?;
  job
    .steps
    .retain(|step| step.context_name.as_deref() == Some("__run"));
  let step = job.steps.first_mut().ok_or("captured script step absent")?;
  set_script(step, PROBE)?;
  step.environment = step_ci.map(|value| mapping("CI", value));
  if let Some(value) = job_ci {
    job.variables.insert(
      "CI".to_owned(),
      VariableValue {
        value: value.to_owned(),
        is_secret: false,
      },
    );
  }
  set_false_github_actions(&mut job);

  run_job(job, &[]).await
}

async fn run_job_events(
  job: AgentJobRequestMessage,
  actions: &[&str],
) -> TestResult<Vec<RunnerEvent>> {
  let root = tempfile::tempdir()?;
  let config = RunnerConfig {
    data_dir: root.path().join("data"),
    workspace_root: root.path().join("work"),
    workspace_gc_hours: 0,
    ..RunnerConfig::default()
  };
  let workspace = config.workspace_root.join(&job.job_id);
  for action in actions {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
      .join("../toolu-runner/tests/fixtures/local_actions")
      .join(action);
    let target = workspace.join(".github/actions").join(action);
    std::fs::create_dir_all(&target)?;
    for entry in std::fs::read_dir(source)? {
      let entry = entry?;
      std::fs::copy(entry.path(), target.join(entry.file_name()))?;
    }
  }
  let runner = Runner::new(config, Arc::new(Mutex::new(SecretMasker::new())));
  let mut events = runner.execute_job(job, CancellationToken::new());
  let mut events_seen = Vec::new();
  // Include real Node startup and workspace teardown under shared-host load.
  tokio::time::timeout(Duration::from_secs(240), async {
    while let Some(event) = events.recv().await {
      events_seen.push(event);
    }
  })
  .await
  .map_err(|error| {
    std::io::Error::new(
      std::io::ErrorKind::TimedOut,
      format!(
        "captured replay timed out: {error}; final events: {:?}",
        events_seen.iter().rev().take(8).collect::<Vec<_>>()
      ),
    )
  })?;
  Ok(events_seen)
}

async fn run_job(job: AgentJobRequestMessage, actions: &[&str]) -> TestResult<Vec<String>> {
  let events = run_job_events(job, actions).await?;
  let conclusion = events.iter().find_map(|event| {
    if let RunnerEvent::JobCompleted { conclusion, .. } = event {
      Some(*conclusion)
    } else {
      None
    }
  });
  assert_eq!(conclusion, Some(Conclusion::Success), "{events:?}");
  Ok(
    events
      .into_iter()
      .filter_map(|event| {
        if let RunnerEvent::Log { line, .. } = event {
          line.starts_with("CI72|").then_some(line)
        } else {
          None
        }
      })
      .collect(),
  )
}

async fn replay_action(action: &str, later_script: Option<&str>) -> TestResult<Vec<String>> {
  let mut job: AgentJobRequestMessage = serde_json::from_str(CAPTURE)?;
  job.steps.retain(|step| {
    step.context_name.as_deref() == Some("__self")
      || later_script.is_some() && step.context_name.as_deref() == Some("__run_3")
  });
  let step = job.steps.first_mut().ok_or("captured action step absent")?;
  step.reference.path = Some(format!("./.github/actions/{action}"));
  step.environment = Some(mapping_pairs(&[("CI", ""), ("GITHUB_ACTIONS", "false")]));
  if let Some(body) = later_script {
    set_script(
      job.steps.get_mut(1).ok_or("captured later script absent")?,
      body,
    )?;
  }
  set_false_github_actions(&mut job);
  let mut actions = vec![action];
  if action == "ci-72-parent" {
    actions.push("ci-72-child");
  } else if matches!(action, "ci-72-nested-node" | "ci-72-inherited-node") {
    actions.push("ci-72-node");
  }
  run_job(job, &actions).await
}

#[tokio::test]
async fn host_script_defaults_and_preserves_ci_precedence() -> TestResult {
  let runner_ci = std::env::var("CI").ok();
  let expected_default = runner_ci.as_deref().unwrap_or("true");
  assert_eq!(
    replay_script(None, None).await?,
    vec![format!("CI72|x|{expected_default}|true")]
  );
  assert_eq!(
    replay_script(Some("job"), None).await?,
    vec!["CI72|x|job|true"]
  );
  assert_eq!(
    replay_script(Some("job"), Some("step")).await?,
    vec!["CI72|x|step|true"]
  );
  assert_eq!(
    replay_script(Some("job"), Some("")).await?,
    vec!["CI72|x||true"]
  );
  Ok(())
}

#[tokio::test]
async fn node_pre_main_post_keep_originating_empty_step_ci() -> TestResult {
  let lines = replay_action(
    "ci-72-node",
    Some("printf 'CI=later\\n' >> \"$GITHUB_ENV\""),
  )
  .await?;
  assert_eq!(lines.len(), 3,);
  for (line, expected) in lines.iter().zip([
    "CI72|node-pre||true|",
    "CI72|node-main||true|",
    "CI72|node-post||true|",
  ]) {
    assert!(line.starts_with(expected), "{line}");
    assert!(
      line.len() > expected.len(),
      "missing process hostname: {line}"
    );
  }
  Ok(())
}

#[tokio::test]
async fn composite_shell_nested_empty_ci_and_forced_github_actions() -> TestResult {
  let lines = replay_action("ci-72-parent", None).await?;
  assert_eq!(lines.len(), 2);
  for (line, expected) in lines.iter().zip([
    "CI72|composite-inner||true|",
    "CI72|composite-parent||true|",
  ]) {
    assert!(line.starts_with(expected), "{line}");
    assert!(
      line.len() > expected.len(),
      "missing process hostname: {line}"
    );
  }
  Ok(())
}

#[tokio::test]
async fn nested_node_post_keeps_its_composite_step_ci() -> TestResult {
  let lines = replay_action(
    "ci-72-nested-node",
    Some("printf 'CI=later\\n' >> \"$GITHUB_ENV\""),
  )
  .await?;
  assert_eq!(lines.len(), 3);
  for (line, expected) in lines.iter().zip([
    "CI72|node-pre||true|",
    "CI72|node-main||true|",
    "CI72|node-post||true|",
  ]) {
    assert!(line.starts_with(expected), "{line}");
    assert!(
      line.len() > expected.len(),
      "missing process hostname: {line}"
    );
  }
  Ok(())
}

#[tokio::test]
async fn nested_node_post_keeps_inherited_parent_step_ci() -> TestResult {
  let lines = replay_action(
    "ci-72-inherited-node",
    Some("printf 'CI=later\\n' >> \"$GITHUB_ENV\""),
  )
  .await?;
  assert_eq!(lines.len(), 3);
  for (line, expected) in lines.iter().zip([
    "CI72|node-pre||true|",
    "CI72|node-main||true|",
    "CI72|node-post||true|",
  ]) {
    assert!(line.starts_with(expected), "{line}");
    assert!(
      line.len() > expected.len(),
      "missing process hostname: {line}"
    );
  }
  Ok(())
}

#[tokio::test]
async fn malformed_action_ci_expression_fails_visibly() -> TestResult {
  let mut job: AgentJobRequestMessage = serde_json::from_str(CAPTURE)?;
  job
    .steps
    .retain(|step| step.context_name.as_deref() == Some("__self"));
  let step = job.steps.first_mut().ok_or("captured action step absent")?;
  step.reference.path = Some("./.github/actions/ci-72-node".to_owned());
  step.environment = Some(TemplateToken {
    token_type: 2,
    d: Some(vec![DictEntry {
      key: literal("CI"),
      value: TemplateToken {
        token_type: 3,
        expr: Some("invalid(".to_owned()),
        ..TemplateToken::default()
      },
    }]),
    ..TemplateToken::default()
  });
  let events = run_job_events(job, &["ci-72-node"]).await?;
  assert!(events.iter().any(|event| matches!(
    event,
    RunnerEvent::JobCompleted {
      conclusion: Conclusion::Failure,
      ..
    }
  )));
  assert!(
    events.iter().any(|event| matches!(
      event,
      RunnerEvent::Log { line, .. } if line.starts_with("##[error]") && line.contains("expression")
    )),
    "{events:?}"
  );
  assert!(!events.iter().any(|event| matches!(
    event,
    RunnerEvent::Log { line, .. } if line.starts_with("CI72|")
  )));
  Ok(())
}
