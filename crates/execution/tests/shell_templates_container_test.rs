//! Opt-in Linux Docker replays for the shell-template container contract.

#![cfg(target_os = "linux")]

use std::error::Error;
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
const COMPOSITE_ACTION: &str = r#"
name: Shell template container probe
description: Executes a custom shell template inside a job container.
runs:
  using: composite
  steps:
    - shell: bash {0} 'composite quoted argument'
      run: |
        test -f "$0"
        test "$1" = 'composite quoted argument'
        case "$0" in /github/runner_temp/*) ;; *) exit 80 ;; esac
        printf 'S80|composite|%s|%s\n' "$1" "$0"
"#;

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

fn set_run_step(job: &mut AgentJobRequestMessage, shell: Option<&str>, script: &str) -> TestResult {
  job
    .steps
    .retain(|step| step.context_name.as_deref() == Some("shell"));
  let step = job.steps.first_mut().ok_or("captured shell step absent")?;
  let mut inputs = vec![("script", script)];
  if let Some(shell) = shell {
    inputs.push(("shell", shell));
  }
  step.inputs = token_map(&inputs);
  Ok(())
}

fn set_composite_step(job: &mut AgentJobRequestMessage) -> TestResult {
  job
    .steps
    .retain(|step| step.context_name.as_deref() == Some("__self"));
  let step = job
    .steps
    .first_mut()
    .ok_or("captured composite step absent")?;
  step.reference.name = None;
  step.reference.git_ref = None;
  step.reference.repository_type = Some("self".to_owned());
  step.reference.path = Some("./.github/actions/shell 80 action".to_owned());
  Ok(())
}

fn captured_job() -> TestResult<AgentJobRequestMessage> {
  Ok(serde_json::from_str(CAPTURE)?)
}

async fn replay(
  job: AgentJobRequestMessage,
  composite: bool,
) -> TestResult<(Conclusion, Vec<String>)> {
  let base = std::env::var_os("TOOLU_CONTAINER_TEST_ROOT")
    .map(std::path::PathBuf::from)
    .ok_or("TOOLU_CONTAINER_TEST_ROOT must name a path shared with Docker")?;
  let root = tempfile::Builder::new()
    .prefix("shell 80 container ")
    .tempdir_in(base)?;
  let config = RunnerConfig {
    data_dir: root.path().join("runner data"),
    workspace_root: root.path().join("work root"),
    workspace_gc_hours: 0,
    ..RunnerConfig::default()
  };
  if composite {
    let action = config
      .workspace_root
      .join(&job.job_id)
      .join(".github/actions/shell 80 action");
    std::fs::create_dir_all(&action)?;
    std::fs::write(action.join("action.yml"), COMPOSITE_ACTION)?;
  }

  let runner = Runner::new(config, Arc::new(Mutex::new(SecretMasker::new())));
  let mut events = runner.execute_job(job, CancellationToken::new());
  let mut lines = Vec::new();
  let mut conclusion = None;
  tokio::time::timeout(Duration::from_secs(120), async {
    while let Some(event) = events.recv().await {
      match event {
        RunnerEvent::Log { line, .. } => lines.push(line),
        RunnerEvent::JobCompleted {
          conclusion: result, ..
        } => conclusion = Some(result),
        RunnerEvent::JobStarted { .. }
        | RunnerEvent::StepStarted { .. }
        | RunnerEvent::StepCompleted { .. }
        | RunnerEvent::StepSkipped { .. }
        | RunnerEvent::LogGroup { .. }
        | RunnerEvent::Annotation { .. } => {},
      }
    }
  })
  .await?;
  Ok((conclusion.ok_or("job completion event absent")?, lines))
}

#[tokio::test]
#[ignore = "requires a Linux runner and real Docker daemon"]
async fn container_default_shell_is_sh_and_pipeline_succeeds() -> TestResult {
  let mut job = captured_job()?;
  set_run_step(
    &mut job,
    None,
    "test -z \"${BASH_VERSION-}\"\nfalse | true\nprintf 'S80|default|%s\\n' \"${BASH_VERSION-}\"",
  )?;
  let (conclusion, lines) = replay(job, false).await?;
  assert_eq!(conclusion, Conclusion::Success, "{lines:#?}");
  assert!(
    lines.iter().any(|line| line == "S80|default|"),
    "{lines:#?}"
  );
  Ok(())
}

#[tokio::test]
#[ignore = "requires a Linux runner and real Docker daemon"]
async fn container_explicit_bash_pipeline_fails() -> TestResult {
  let mut job = captured_job()?;
  set_run_step(&mut job, Some("bash"), "false | true")?;
  let (conclusion, lines) = replay(job, false).await?;
  assert_eq!(conclusion, Conclusion::Failure, "{lines:#?}");
  Ok(())
}

#[tokio::test]
#[ignore = "requires a Linux runner and real Docker daemon"]
async fn container_custom_template_preserves_quoted_argument_and_mounted_script_path() -> TestResult
{
  let mut job = captured_job()?;
  set_run_step(
    &mut job,
    Some("bash {0} 'quoted argument'"),
    "test -f \"$0\"\ntest \"$1\" = 'quoted argument'\ncase \"$0\" in /github/runner_temp/*) ;; *) exit 80 ;; esac\nprintf 'S80|custom|%s|%s\\n' \"$1\" \"$0\"",
  )?;
  let (conclusion, lines) = replay(job, false).await?;
  assert_eq!(conclusion, Conclusion::Success, "{lines:#?}");
  assert!(
    // The host temp root deliberately contains spaces, but PathTranslator
    // normalizes it to this mounted container path before template substitution.
    lines
      .iter()
      .any(|line| line.starts_with("S80|custom|quoted argument|/github/runner_temp/")),
    "{lines:#?}"
  );
  Ok(())
}

#[tokio::test]
#[ignore = "requires a Linux runner and real Docker daemon"]
async fn container_composite_custom_template_uses_mounted_script_path() -> TestResult {
  let mut job = captured_job()?;
  set_composite_step(&mut job)?;
  let (conclusion, lines) = replay(job, true).await?;
  assert_eq!(conclusion, Conclusion::Success, "{lines:#?}");
  assert!(
    lines.iter().any(|line| {
      line.starts_with("S80|composite|composite quoted argument|/github/runner_temp/")
    }),
    "{lines:#?}"
  );
  Ok(())
}

#[tokio::test]
#[ignore = "requires a Linux runner and real Docker daemon"]
async fn container_unknown_shell_fails_before_running_body() -> TestResult {
  let mut job = captured_job()?;
  set_run_step(&mut job, Some("unknown80"), "printf 'S80|body-ran\\n'")?;
  let (conclusion, lines) = replay(job, false).await?;
  assert_eq!(conclusion, Conclusion::Failure, "{lines:#?}");
  assert!(
    !lines.iter().any(|line| line == "S80|body-ran"),
    "{lines:#?}"
  );
  Ok(())
}
