//! Captured Linux job-container replay of flags observed by real Docker children.

#![cfg(target_os = "linux")]

use std::error::Error;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use execution::Runner;
use shared::{
  AgentJobRequestMessage, Conclusion, DictEntry, RunnerConfig, RunnerEvent, SecretMasker,
  TemplateToken,
};
use tokio_util::sync::CancellationToken;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;
const CAPTURE: &str = include_str!("../../toolu-runner/tests/fixtures/job_container_message.json");
const ACTIONS: &str = concat!(
  env!("CARGO_MANIFEST_DIR"),
  "/../toolu-runner/tests/fixtures/local_actions"
);
const SHELL: &str = "printf 'CI72|container-shell|%s|%s|%s\\n' \"${CI-<missing>}\" \"${GITHUB_ACTIONS-<missing>}\" \"$(hostname)\"";
const VERIFY: &str = "printf 'CI72|container-verify|%s|%s|%s\\n' \"${CI-<missing>}\" \"${GITHUB_ACTIONS-<missing>}\" \"$(hostname)\"";

fn literal(value: &str) -> TemplateToken {
  TemplateToken {
    token_type: 0,
    lit: Some(value.to_owned()),
    ..TemplateToken::default()
  }
}

fn token_map(entries: &[(&str, &str)]) -> TemplateToken {
  TemplateToken {
    token_type: 2,
    d: Some(
      entries
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

fn set_script(step: &mut shared::ActionStep, script: &str) -> TestResult {
  let value = step
    .inputs
    .d
    .as_mut()
    .and_then(|entries| {
      entries
        .iter_mut()
        .find(|entry| entry.key.to_string_value() == Some("script"))
    })
    .ok_or("captured script input absent")?;
  value.value = literal(script);
  Ok(())
}

fn captured_job(
  with_actions: bool,
  container_ci: Option<&str>,
) -> TestResult<AgentJobRequestMessage> {
  let mut job: AgentJobRequestMessage = serde_json::from_str(CAPTURE)?;
  job.steps.retain(|step| {
    matches!(
      step.context_name.as_deref(),
      Some("shell" | "node_probe" | "__self" | "__run")
    ) && (with_actions || step.context_name.as_deref() == Some("shell"))
  });
  let container = job
    .job_container
    .as_mut()
    .ok_or("captured jobContainer absent")?;
  let env = container
    .d
    .as_mut()
    .and_then(|entries| {
      entries
        .iter_mut()
        .find(|entry| entry.key.to_string_value() == Some("env"))
    })
    .ok_or("captured container env absent")?;
  let values = env
    .value
    .d
    .as_mut()
    .ok_or("captured container env map absent")?;
  values.push(DictEntry {
    key: literal("GITHUB_ACTIONS"),
    value: literal("false"),
  });
  if let Some(ci) = container_ci {
    values.push(DictEntry {
      key: literal("CI"),
      value: literal(ci),
    });
  }
  let shell = job.steps.first_mut().ok_or("captured shell step absent")?;
  set_script(shell, SHELL)?;
  if with_actions {
    let node = job.steps.get_mut(1).ok_or("captured node step absent")?;
    node.reference.name = None;
    node.reference.git_ref = None;
    node.reference.repository_type = Some("self".to_owned());
    node.reference.path = Some("./.github/actions/ci-72-node".to_owned());
    node.environment = Some(token_map(&[("CI", ""), ("GITHUB_ACTIONS", "false")]));
    let composite = job
      .steps
      .get_mut(2)
      .ok_or("captured composite step absent")?;
    composite.reference.path = Some("./.github/actions/ci-72-parent".to_owned());
    composite.environment = Some(token_map(&[("CI", ""), ("GITHUB_ACTIONS", "false")]));
    set_script(
      job.steps.get_mut(3).ok_or("captured verify step absent")?,
      VERIFY,
    )?;
  }
  Ok(job)
}

async fn replay(job: AgentJobRequestMessage, actions: &[&str]) -> TestResult<Vec<String>> {
  let base = std::env::var_os("TOOLU_CONTAINER_TEST_ROOT")
    .map(std::path::PathBuf::from)
    .ok_or("TOOLU_CONTAINER_TEST_ROOT must name a path shared with Docker")?;
  let root = tempfile::Builder::new()
    .prefix("ci72 container ")
    .tempdir_in(base)?;
  let config = RunnerConfig {
    data_dir: root.path().join("runner data"),
    workspace_root: root.path().join("work root"),
    workspace_gc_hours: 0,
    ..RunnerConfig::default()
  };
  let workspace = config.workspace_root.join(&job.job_id);
  for action in actions {
    let source = Path::new(ACTIONS).join(action);
    let target = workspace.join(".github/actions").join(action);
    std::fs::create_dir_all(&target)?;
    for entry in std::fs::read_dir(source)? {
      let entry = entry?;
      std::fs::copy(entry.path(), target.join(entry.file_name()))?;
    }
  }
  let runner = Runner::new(config, Arc::new(Mutex::new(SecretMasker::new())));
  let mut events = runner.execute_job(job, CancellationToken::new());
  let mut lines = Vec::new();
  let mut conclusion = None;
  tokio::time::timeout(Duration::from_secs(240), async {
    while let Some(event) = events.recv().await {
      if let RunnerEvent::Log { line, .. } = &event
        && line.starts_with("CI72|")
      {
        lines.push(line.clone());
      }
      if let RunnerEvent::JobCompleted {
        conclusion: result, ..
      } = event
      {
        conclusion = Some(result);
      }
    }
  })
  .await?;
  assert_eq!(conclusion, Some(Conclusion::Success));
  Ok(lines)
}

#[tokio::test]
#[ignore = "requires a Linux runner and real Docker daemon"]
async fn container_defaults_to_runner_ci_or_true() -> TestResult {
  let expected = std::env::var("CI").unwrap_or_else(|_| "true".to_owned());
  let lines = replay(captured_job(false, None)?, &[]).await?;
  assert_eq!(
    lines,
    [format!(
      "CI72|container-shell|{expected}|true|container-73-probe"
    )]
  );
  Ok(())
}

#[tokio::test]
#[ignore = "requires a Linux runner and real Docker daemon"]
async fn container_preserves_declaration_and_step_overrides_across_handlers() -> TestResult {
  let lines = replay(
    captured_job(true, Some("false"))?,
    &["ci-72-node", "ci-72-parent", "ci-72-child"],
  )
  .await?;
  assert_eq!(
    lines,
    [
      "CI72|container-shell|false|true|container-73-probe",
      "CI72|node-pre||true|container-73-probe",
      "CI72|node-main||true|container-73-probe",
      "CI72|composite-inner||true|container-73-probe",
      "CI72|composite-parent||true|container-73-probe",
      "CI72|container-verify|false|true|container-73-probe",
      "CI72|node-post||true|container-73-probe",
    ]
  );
  Ok(())
}
