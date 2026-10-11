//! Issue 88: "Complete job" always closes its row and masks what it writes,
//! driven with the sanitized `completejob-88.yml` env capture (run
//! 38098611227) and real job contexts.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use shared::{
  AgentJobRequestMessage, AnnotationLevel, Conclusion, DictEntry, RunnerEvent, SecretMasker,
  TemplateToken,
};
use tokio::sync::mpsc;

use super::complete_step::run_complete_step;
use crate::execution::context::ExecutionContext;
use crate::execution::job_spec::JobSpec;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

const ENV_CAPTURE: &str = include_str!("../../../tests/completejob_88_env_message.json");
/// A registered secret that is also a valid expression identifier, so an
/// "unknown named value" error echoes it.
const SECRET: &str = "s3cr3tctx";

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
    ..TemplateToken::default()
  }
}

fn outputs(entries: Vec<(&str, TemplateToken)>) -> TemplateToken {
  TemplateToken {
    token_type: 2,
    d: Some(
      entries
        .into_iter()
        .map(|(name, value)| DictEntry {
          key: literal(name),
          value,
        })
        .collect(),
    ),
    ..TemplateToken::default()
  }
}

fn masked_context() -> ExecutionContext {
  let mut masker = SecretMasker::new();
  masker.add_secret(SECRET);
  ExecutionContext::with_masker(Arc::new(Mutex::new(masker)))
}

/// Run "Complete job"; returns its result and every event it emitted.
async fn complete(
  spec: &JobSpec,
  message: &AgentJobRequestMessage,
  ctx: &ExecutionContext,
) -> (Result<Conclusion, shared::RunnerError>, Vec<RunnerEvent>) {
  let (tx, mut rx) = mpsc::channel(256);
  let result = run_complete_step(spec, message, ctx, &tx, Conclusion::Success)
    .await
    .map(|result| result.conclusion);
  drop(tx);
  let mut events = Vec::new();
  while let Some(event) = rx.recv().await {
    events.push(event);
  }
  (result, events)
}

/// `(log lines, annotations, row conclusion)` of the "Complete job" row.
fn row(events: &[RunnerEvent]) -> (Vec<&str>, Vec<(AnnotationLevel, &str)>, Option<Conclusion>) {
  let (mut logs, mut annotations, mut conclusion) = (Vec::new(), Vec::new(), None);
  for event in events {
    if let RunnerEvent::Log { line, .. } = event {
      logs.push(line.as_str());
    }
    if let RunnerEvent::Annotation { level, message, .. } = event {
      annotations.push((*level, message.as_str()));
    }
    if let RunnerEvent::StepCompleted {
      conclusion: done, ..
    } = event
    {
      conclusion = Some(*done);
    }
  }
  (logs, annotations, conclusion)
}

#[tokio::test]
async fn local_output_error_still_closes_the_row_as_failed() -> TestResult {
  let message: AgentJobRequestMessage = serde_json::from_str(ENV_CAPTURE)?;
  let outputs = HashMap::from([("bad".to_owned(), "${{ fromJSON('{') }}".to_owned())]);
  let spec = JobSpec::from_workflow(outputs, None, None);
  let (result, events) = complete(&spec, &message, &ExecutionContext::new_for_test()).await;
  assert!(result.is_err(), "the local output error propagates");
  let (_, annotations, conclusion) = row(&events);
  assert_eq!(conclusion, Some(Conclusion::Failure));
  assert!(
    matches!(annotations.as_slice(), [(AnnotationLevel::Error, message)] if message.contains("JSON")),
    "{annotations:?}"
  );
  Ok(())
}

#[tokio::test]
async fn output_names_are_masked_in_the_set_output_line() -> TestResult {
  let message: AgentJobRequestMessage = serde_json::from_str(ENV_CAPTURE)?;
  let spec = JobSpec {
    acquired_outputs: Some(outputs(vec![(&format!("name-{SECRET}"), literal("plain"))])),
    ..JobSpec::default()
  };
  let mut message = message;
  message.actions_environment = None;
  let (result, events) = complete(&spec, &message, &masked_context()).await;
  assert_eq!(result.ok(), Some(Conclusion::Success));
  let (logs, _, conclusion) = row(&events);
  assert_eq!(conclusion, Some(Conclusion::Success));
  assert!(logs.contains(&"Set output 'name-***'"), "{logs:?}");
  assert!(logs.iter().all(|line| !line.contains(SECRET)), "{logs:?}");
  Ok(())
}

#[tokio::test]
async fn output_and_environment_url_errors_are_masked() -> TestResult {
  let mut message: AgentJobRequestMessage = serde_json::from_str(ENV_CAPTURE)?;
  message
    .actions_environment
    .as_mut()
    .ok_or("captured actionsEnvironment missing")?
    .url = Some(expression(&format!("{SECRET}.url")));
  let spec = JobSpec {
    acquired_outputs: Some(outputs(vec![("bad", expression(&format!("{SECRET}.out")))])),
    ..JobSpec::default()
  };
  let (result, events) = complete(&spec, &message, &masked_context()).await;
  assert_eq!(result.ok(), Some(Conclusion::Failure));
  let (logs, annotations, conclusion) = row(&events);
  assert_eq!(conclusion, Some(Conclusion::Failure));
  let messages: Vec<&str> = annotations.iter().map(|(_, message)| *message).collect();
  assert_eq!(
    messages.first().copied(),
    Some("Fail to evaluate job outputs")
  );
  assert!(messages.contains(&"Failed to evaluate environment url"));
  // Both evaluation errors name the unknown context, which is the secret.
  assert_eq!(
    messages
      .iter()
      .filter(|message| message.contains("***"))
      .count(),
    2,
    "{messages:?}"
  );
  for text in logs.iter().chain(&messages) {
    assert!(!text.contains(SECRET), "unmasked: {text}");
  }
  Ok(())
}
