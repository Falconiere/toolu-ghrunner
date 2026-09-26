//! #80 replay: retain captured wire structure/IDs, substitute shell probe tokens.

use execution::Runner;
use shared::{
  AgentJobRequestMessage, Conclusion, DictEntry, RunnerConfig, RunnerEvent, SecretMasker,
  TemplateToken,
};
use std::error::Error;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;
const CAPTURE: &str = include_str!("incoming_contexts_matrix_0.json");

fn literal(text: &str) -> TemplateToken {
  TemplateToken {
    token_type: 0,
    lit: Some(text.to_owned()),
    ..TemplateToken::default()
  }
}
fn map(entries: Vec<(&str, TemplateToken)>) -> TemplateToken {
  TemplateToken {
    token_type: 2,
    d: Some(
      entries
        .into_iter()
        .map(|(key, value)| DictEntry {
          key: literal(key),
          value,
        })
        .collect(),
    ),
    ..TemplateToken::default()
  }
}

async fn replay(
  shell: Option<&str>,
  default: Option<&str>,
  script: &str,
  composite: bool,
) -> TestResult<Vec<RunnerEvent>> {
  let mut job: AgentJobRequestMessage = serde_json::from_str(CAPTURE)?;
  job.steps.retain(|step| {
    step.context_name.as_deref() == Some(if composite { "__self" } else { "__run" })
  });
  job.defaults = default
    .map(|shell| vec![map(vec![("run", map(vec![("shell", literal(shell))]))])])
    .unwrap_or_default();
  let step = job.steps.first_mut().ok_or("captured step")?;
  if composite {
    step.reference.path = Some("./.github/actions/shell80".to_owned());
  } else {
    let mut entries = vec![("script", literal(script))];
    if let Some(shell) = shell {
      entries.push(("shell", literal(shell)));
    }
    step.inputs = map(entries);
  }
  let root = tempfile::Builder::new()
    .prefix("shell 80 replay ")
    .tempdir()?;
  let config = RunnerConfig {
    data_dir: root.path().join("data"),
    workspace_root: root.path().join("work"),
    workspace_gc_hours: 0,
    ..RunnerConfig::default()
  };
  if composite {
    let dir = config
      .workspace_root
      .join(&job.job_id)
      .join(".github/actions/shell80");
    std::fs::create_dir_all(&dir)?;
    let manifest = serde_json::json!({"name":"shell80", "description":"real shell contract probe", "runs":{"using":"composite", "steps":[{"shell":shell, "run":script}]}});
    std::fs::write(dir.join("action.yml"), serde_yaml::to_string(&manifest)?)?;
  }
  let runner = Runner::new(config, Arc::new(Mutex::new(SecretMasker::new())));
  let mut events = runner.execute_job(job, CancellationToken::new());
  let mut seen = Vec::new();
  tokio::time::timeout(Duration::from_secs(60), async {
    while let Some(event) = events.recv().await {
      seen.push(event);
    }
  })
  .await?;
  Ok(seen)
}

fn conclusion(events: &[RunnerEvent], expected: Conclusion) {
  assert!(
    events.iter().any(
      |e| matches!(e, RunnerEvent::JobCompleted { conclusion, .. } if *conclusion == expected)
    ),
    "{events:?}"
  );
}
fn marker(events: &[RunnerEvent], expected: &str) {
  assert!(
    events
      .iter()
      .any(|e| matches!(e, RunnerEvent::Log { line, .. } if line == expected)),
    "{events:?}"
  );
}

#[tokio::test]
async fn captured_default_pipeline_and_explicit_bash_have_different_results() -> TestResult {
  conclusion(
    &replay(None, None, "false | true", false).await?,
    Conclusion::Success,
  );
  conclusion(
    &replay(Some("bash"), None, "false | true", false).await?,
    Conclusion::Failure,
  );
  Ok(())
}
#[tokio::test]
async fn captured_job_default_template_and_explicit_override() -> TestResult {
  let events = replay(
    None,
    Some("perl {0} 'job default'"),
    "print join('|', @ARGV), qq(\\n);",
    false,
  )
  .await?;
  conclusion(&events, Conclusion::Success);
  marker(&events, "job default");
  let events = replay(
    Some("sh {0} 'step override'"),
    Some("perl {0}"),
    "printf '%s\\n' \"$1\"",
    false,
  )
  .await?;
  conclusion(&events, Conclusion::Success);
  marker(&events, "step override");
  Ok(())
}
#[tokio::test]
async fn captured_unknown_shell_reports_failure_without_executing_body() -> TestResult {
  let events = replay(Some("unknown80"), None, "echo SHOULD_NOT_RUN", false).await?;
  conclusion(&events, Conclusion::Failure);
  assert!(events.iter().any(|e| matches!(e, RunnerEvent::Log { line, .. } if line.contains("shell") && line.contains("{0}"))), "{events:?}");
  assert!(
    !events
      .iter()
      .any(|e| matches!(e, RunnerEvent::Log { line, .. } if line == "SHOULD_NOT_RUN"))
  );
  Ok(())
}
#[tokio::test]
async fn captured_composite_custom_shell_ignores_job_default() -> TestResult {
  let events = replay(
    Some("perl {0} 'composite argument'"),
    Some("bash"),
    "print join('|', @ARGV), qq(\\n);",
    true,
  )
  .await?;
  conclusion(&events, Conclusion::Success);
  marker(&events, "composite argument");
  let events = replay(Some("unknown80"), None, "echo SHOULD_NOT_RUN", true).await?;
  conclusion(&events, Conclusion::Failure);
  Ok(())
}
