//! Issue 88: the "Complete job" step evaluates `environment.url` after every
//! main and post step, replaying the sanitized github.com acquisitions from
//! `completejob-88.yml` run 38098611227 (toolu lane) through the real engine
//! and real `bash`.
//!
//! Boundary cases are capture-derived: they rewrite only the captured
//! `actionsEnvironment.url` token (or a step script) of the real message.

use std::collections::HashMap;
use std::error::Error;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use execution::Runner;
use shared::{
  ActionsEnvironment, AgentJobRequestMessage, AnnotationLevel, Conclusion, MaskHint, RunnerConfig,
  RunnerEvent, SecretMasker, TemplateToken,
};
use tokio_util::sync::CancellationToken;

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

const ENV_CAPTURE: &str = include_str!("completejob_88_env_message.json");
const SECRET_CAPTURE: &str = include_str!("completejob_88_secret_message.json");
/// `format('https://toolu-88.example/{0}/{1}', matrix.lane, github.run_id)`.
const CAPTURED_URL: &str = "https://toolu-88.example/toolu/38098611227";

fn env_message() -> TestResult<AgentJobRequestMessage> {
  Ok(serde_json::from_str(ENV_CAPTURE)?)
}

fn with_url(token: TemplateToken) -> TestResult<AgentJobRequestMessage> {
  let mut message = env_message()?;
  let environment = message
    .actions_environment
    .as_mut()
    .ok_or("captured actionsEnvironment missing")?;
  environment.url = Some(token);
  Ok(message)
}

fn literal(text: &str) -> TemplateToken {
  TemplateToken {
    token_type: 0,
    lit: Some(text.to_owned()),
    ..TemplateToken::default()
  }
}

fn expression(expr: &str) -> TemplateToken {
  TemplateToken {
    token_type: 3,
    expr: Some(expr.to_owned()),
    file: Some(1),
    line: Some(29),
    col: Some(12),
    ..TemplateToken::default()
  }
}

fn set_script(message: &mut AgentJobRequestMessage, index: usize, script: &str) -> TestResult {
  let step = message
    .steps
    .get_mut(index)
    .ok_or("captured step missing")?;
  let input = step
    .inputs
    .d
    .as_mut()
    .ok_or("captured inputs missing")?
    .iter_mut()
    .find(|entry| entry.key.to_string_value() == Some("script"))
    .ok_or("captured script missing")?;
  input.value = literal(script);
  Ok(())
}

async fn run(message: AgentJobRequestMessage, cancel_on: Option<&str>) -> TestResult<Job> {
  let dir = tempfile::tempdir()?;
  let config = RunnerConfig {
    data_dir: dir.path().join("data"),
    workspace_root: dir.path().join("work"),
    workspace_gc_hours: 0,
    ..RunnerConfig::default()
  };
  let cancel = CancellationToken::new();
  let runner = Runner::new(config, Arc::new(Mutex::new(SecretMasker::new())));
  let mut stream = runner.execute_job(message, cancel.clone());
  let cancel_on = cancel_on.map(str::to_owned);
  let events = tokio::time::timeout(Duration::from_secs(90), async move {
    let mut events = Vec::new();
    while let Some(event) = stream.recv().await {
      if let RunnerEvent::StepStarted { step_id, .. } = &event
        && cancel_on.as_deref() == Some(step_id.as_str())
      {
        cancel.cancel();
      }
      events.push(event);
    }
    events
  })
  .await?;
  Job::from_events(events)
}

/// The observable result of one replayed job.
struct Job {
  events: Vec<RunnerEvent>,
  conclusion: Conclusion,
  environment_url: Option<String>,
  complete_id: String,
  numbers: HashMap<String, u32>,
}

impl Job {
  fn from_events(events: Vec<RunnerEvent>) -> TestResult<Self> {
    let mut completion = None;
    let mut complete_id = None;
    let mut numbers = HashMap::new();
    for event in &events {
      if let RunnerEvent::JobCompleted {
        conclusion,
        environment_url,
        ..
      } = event
      {
        completion = Some((*conclusion, environment_url.clone()));
      }
      if let RunnerEvent::StepStarted {
        step_id,
        step_name,
        step_number,
      } = event
      {
        numbers.insert(step_id.clone(), *step_number);
        if step_name == "Complete job" {
          complete_id = Some(step_id.clone());
        }
      }
    }
    let (conclusion, environment_url) = completion.ok_or("JobCompleted missing")?;
    Ok(Self {
      events,
      conclusion,
      environment_url,
      complete_id: complete_id.ok_or("Complete job step missing")?,
      numbers,
    })
  }

  fn logs(&self, step: &str) -> Vec<&str> {
    self
      .events
      .iter()
      .filter_map(|event| {
        if let RunnerEvent::Log { step_id, line, .. } = event
          && step_id == step
        {
          Some(line.as_str())
        } else {
          None
        }
      })
      .collect()
  }

  fn complete_logs(&self) -> Vec<&str> {
    self.logs(&self.complete_id)
  }

  fn complete_annotations(&self) -> Vec<(AnnotationLevel, &str)> {
    self
      .events
      .iter()
      .filter_map(|event| {
        if let RunnerEvent::Annotation {
          step_id,
          level,
          message,
          ..
        } = event
          && *step_id == self.complete_id
        {
          Some((*level, message.as_str()))
        } else {
          None
        }
      })
      .collect()
  }

  fn complete_conclusion(&self) -> Option<Conclusion> {
    self.events.iter().find_map(|event| {
      if let RunnerEvent::StepCompleted {
        step_id,
        conclusion,
        ..
      } = event
        && *step_id == self.complete_id
      {
        Some(*conclusion)
      } else {
        None
      }
    })
  }

  fn complete_metadata(&self) -> Option<(&str, Option<&str>)> {
    self.events.iter().find_map(|event| {
      if let RunnerEvent::StepMetadata {
        step_id,
        kind,
        action,
        ..
      } = event
        && *step_id == self.complete_id
      {
        Some((kind.as_str(), action.as_deref()))
      } else {
        None
      }
    })
  }
}

#[tokio::test]
async fn captured_url_is_evaluated_in_complete_job_after_every_step() -> TestResult {
  let message = env_message()?;
  let steps: Vec<String> = message.steps.iter().map(|step| step.id.clone()).collect();
  let job = run(message, None).await?;
  assert_eq!(job.conclusion, Conclusion::Success);
  assert_eq!(job.environment_url.as_deref(), Some(CAPTURED_URL));
  assert_eq!(
    job.complete_logs(),
    [
      "Evaluate and set environment url",
      format!("Evaluated environment url: {CAPTURED_URL}").as_str()
    ]
  );
  assert_eq!(job.complete_conclusion(), Some(Conclusion::Success));
  assert_eq!(
    job.complete_metadata(),
    Some(("runner", Some("complete_job")))
  );
  // "Complete job" is the last row: after the captured steps 2 and 3.
  let complete = job.numbers.get(&job.complete_id).copied();
  assert_eq!(complete, Some(4));
  for id in &steps {
    assert!(
      job.numbers.get(id).copied() < complete,
      "{id} numbered after Complete job"
    );
  }
  // The URL is evaluated after the steps that produce it.
  let last_step = job.events.iter().rposition(
    |event| matches!(event, RunnerEvent::StepCompleted { step_id, .. } if steps.contains(step_id)),
  );
  let evaluated = job.events.iter().position(|event| {
    matches!(event, RunnerEvent::Log { line, .. } if line.starts_with("Evaluated environment url"))
  });
  assert!(
    last_step < evaluated,
    "URL evaluated before the last step completed"
  );
  Ok(())
}

#[tokio::test]
async fn absent_environment_or_null_url_reports_nothing() -> TestResult {
  let mut absent = env_message()?;
  absent.actions_environment = None;
  let mut null = env_message()?;
  if let Some(environment) = null.actions_environment.as_mut() {
    environment.url = Some(TemplateToken {
      token_type: 7,
      ..TemplateToken::default()
    });
  }
  for message in [absent, null] {
    let job = run(message, None).await?;
    assert_eq!(job.conclusion, Conclusion::Success);
    assert_eq!(job.environment_url, None);
    assert!(job.complete_logs().is_empty(), "{:?}", job.complete_logs());
  }
  Ok(())
}

#[tokio::test]
async fn scalar_results_are_sent_as_upstream_strings() -> TestResult {
  let cases = [
    (
      literal("https://literal.example/app"),
      "https://literal.example/app",
    ),
    (literal(""), ""),
    (literal("not a url"), "not a url"),
    (expression("steps.deploy.outputs.missing"), ""),
    (expression("1"), "1"),
    (expression("true"), "true"),
    (expression("null"), ""),
  ];
  for (token, expected) in cases {
    let job = run(with_url(token)?, None).await?;
    assert_eq!(job.conclusion, Conclusion::Success);
    assert_eq!(job.environment_url.as_deref(), Some(expected));
  }
  Ok(())
}

#[tokio::test]
async fn url_reads_the_final_env_written_through_github_env() -> TestResult {
  let mut message = with_url(expression("format('https://{0}/app', env.TARGET_HOST)"))?;
  set_script(
    &mut message,
    0,
    "echo TARGET_HOST=env-host.example >> \"$GITHUB_ENV\"",
  )?;
  let job = run(message, None).await?;
  assert_eq!(
    job.environment_url.as_deref(),
    Some("https://env-host.example/app")
  );
  Ok(())
}

#[tokio::test]
async fn invalid_results_fail_complete_job_and_the_job() -> TestResult {
  // toolu's evaluator words an unknown context `unknown named value: secrets`
  // where upstream says `Unrecognized named-value: 'secrets'`; the outcome
  // (template error, failed job) is the same.
  let cases = [
    (
      expression("fromJSON('{\"a\":1}')"),
      "A mapping was not expected",
    ),
    (expression("fromJSON('[1]')"), "A sequence was not expected"),
    (expression("secrets.GITHUB_TOKEN"), "secrets"),
    (expression("fromJSON('{')"), "fromJSON"),
  ];
  for (token, detail) in cases {
    let job = run(with_url(token)?, None).await?;
    assert_eq!(job.conclusion, Conclusion::Failure, "{detail}");
    assert_eq!(job.environment_url, None);
    assert_eq!(job.complete_conclusion(), Some(Conclusion::Failure));
    let logs = job.complete_logs();
    assert!(
      logs.contains(&"##[error]Failed to evaluate environment url"),
      "{logs:?}"
    );
    assert!(
      logs.iter().any(
        |line| line.starts_with("##[error]The template is not valid. ") && line.contains(detail)
      ),
      "{detail}: {logs:?}"
    );
    let errors = job
      .complete_annotations()
      .into_iter()
      .filter(|(level, _)| *level == AnnotationLevel::Error)
      .count();
    assert_eq!(errors, 2);
  }
  Ok(())
}

#[tokio::test]
async fn captured_runtime_masked_url_is_suppressed_with_a_warning() -> TestResult {
  let message: AgentJobRequestMessage = serde_json::from_str(SECRET_CAPTURE)?;
  let name = message
    .actions_environment
    .as_ref()
    .and_then(|environment| environment.name.clone())
    .ok_or("captured environment name missing")?;
  let job = run(message, None).await?;
  assert_eq!(job.conclusion, Conclusion::Success);
  assert_eq!(job.environment_url, None);
  let warning = format!("Skip setting environment url as environment '{name}' may contain secret.");
  assert_eq!(
    job.complete_annotations(),
    [(AnnotationLevel::Warning, warning.as_str())]
  );
  let logs = job.complete_logs();
  assert!(
    logs.contains(&format!("##[warning]{warning}").as_str()),
    "{logs:?}"
  );
  assert!(
    !logs
      .iter()
      .any(|line| line.starts_with("Evaluated environment url"))
  );
  assert_eq!(job.complete_conclusion(), Some(Conclusion::Success));
  Ok(())
}

#[tokio::test]
async fn mask_hint_and_literal_secret_urls_are_suppressed() -> TestResult {
  let hint = MaskHint {
    value: "toolu-88.example".to_owned(),
    mask_type: Some("regex".to_owned()),
  };
  let mut hinted = env_message()?;
  hinted.mask.push(hint.clone());
  let mut literal_secret = with_url(literal("https://toolu-88.example/literal"))?;
  literal_secret.mask.push(hint);
  for message in [hinted, literal_secret] {
    let job = run(message, None).await?;
    assert_eq!(job.environment_url, None);
    assert_eq!(job.complete_annotations().len(), 1);
  }
  Ok(())
}

#[tokio::test]
async fn absent_environment_name_is_quoted_empty() -> TestResult {
  let mut message = env_message()?;
  message.actions_environment = Some(ActionsEnvironment {
    name: None,
    url: Some(literal("https://toolu-88.example/x")),
  });
  message.mask.push(MaskHint {
    value: "toolu-88.example".to_owned(),
    mask_type: Some("regex".to_owned()),
  });
  let job = run(message, None).await?;
  assert_eq!(
    job.complete_annotations(),
    [(
      AnnotationLevel::Warning,
      "Skip setting environment url as environment '' may contain secret."
    )]
  );
  Ok(())
}

#[tokio::test]
async fn cancelled_job_still_reports_the_url_from_completed_steps() -> TestResult {
  let mut message = env_message()?;
  set_script(&mut message, 1, "sleep 30")?;
  let sleeper = message
    .steps
    .get(1)
    .ok_or("captured second step missing")?
    .id
    .clone();
  let job = run(message, Some(&sleeper)).await?;
  assert_eq!(job.conclusion, Conclusion::Cancelled);
  assert_eq!(job.environment_url.as_deref(), Some(CAPTURED_URL));
  assert_eq!(job.complete_conclusion(), Some(Conclusion::Success));
  Ok(())
}
