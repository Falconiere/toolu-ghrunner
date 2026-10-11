//! The "Complete job" step (upstream `JobExtension.FinalizeJob`): job outputs,
//! then the deployment environment URL, reported as the job's last row.

use std::collections::HashMap;

use shared::{
  AgentJobRequestMessage, AnnotationLevel, Conclusion, LogStream, RunnerError, RunnerEvent,
};
use tokio::sync::mpsc;

use super::outputs::evaluate_final_outputs;
use crate::execution::context::ExecutionContext;
use crate::execution::environment_url::{EnvironmentUrl, evaluate_environment_url};
use crate::execution::job_spec::JobSpec;
use crate::execution::post_drain::post_number;

/// What a job body reports once "Complete job" has run.
pub(crate) struct JobResult {
  /// The job conclusion after output and URL evaluation.
  pub(crate) conclusion: Conclusion,
  /// Resolved job outputs.
  pub(crate) outputs: HashMap<String, String>,
  /// The evaluated, secret-free `environment.url`.
  pub(crate) environment_url: Option<String>,
}

/// The reported "Complete job" row and the event sender its lines go to.
pub(super) struct CompleteStep<'a> {
  id: String,
  events: &'a mpsc::Sender<RunnerEvent>,
  masker: &'a ExecutionContext,
}

impl CompleteStep<'_> {
  async fn send(&self, event: RunnerEvent) {
    if self.events.send(event).await.is_err() {
      tracing::warn!("event channel closed; complete-job event was dropped");
    }
  }

  /// Write one output line to the step log.
  pub(super) async fn log(&self, line: &str) {
    self
      .send(RunnerEvent::Log {
        step_id: self.id.clone(),
        line: line.to_owned(),
        stream: LogStream::Stdout,
      })
      .await;
  }

  /// Report an upstream-style issue: a `##[level]` log line plus an
  /// annotation on this step.
  pub(super) async fn issue(&self, level: AnnotationLevel, message: &str) {
    let prefix = match level {
      AnnotationLevel::Notice => "notice",
      AnnotationLevel::Warning => "warning",
      AnnotationLevel::Error => "error",
    };
    self.log(&format!("##[{prefix}]{message}")).await;
    self
      .send(RunnerEvent::Annotation {
        step_id: self.id.clone(),
        level,
        message: message.to_owned(),
        file: None,
        line: None,
        end_line: None,
        col: None,
        end_column: None,
        title: None,
      })
      .await;
  }

  /// Close the row with `conclusion`.
  async fn complete(&self, conclusion: Conclusion) {
    self
      .send(RunnerEvent::StepCompleted {
        step_id: self.id.clone(),
        conclusion,
        outputs: HashMap::new(),
      })
      .await;
  }

  /// Mask a name before it is embedded in a message.
  pub(super) fn mask(&self, text: &str) -> String {
    match self.masker.masker().lock() {
      Ok(guard) => guard.mask(text).into_owned(),
      Err(poisoned) => poisoned.into_inner().mask(text).into_owned(),
    }
  }
}

/// Run "Complete job": evaluate outputs and the environment URL as one
/// reported step numbered after every main and post row.
///
/// # Errors
///
/// Propagates a local (non-acquired) job output expression error after
/// closing the row as failed.
pub(super) async fn run_complete_step(
  spec: &JobSpec,
  msg: &AgentJobRequestMessage,
  ctx: &ExecutionContext,
  events: &mpsc::Sender<RunnerEvent>,
  conclusion: Conclusion,
) -> Result<JobResult, RunnerError> {
  let step = CompleteStep {
    id: uuid::Uuid::new_v4().to_string(),
    events,
    masker: ctx,
  };
  let number = ctx
    .next_step_number
    .unwrap_or_else(|| post_number(2, msg.steps.len()));
  step
    .send(RunnerEvent::StepStarted {
      step_id: step.id.clone(),
      step_name: "Complete job".to_owned(),
      step_number: number,
    })
    .await;
  step
    .send(RunnerEvent::StepMetadata {
      step_id: step.id.clone(),
      kind: "runner".to_owned(),
      action: Some("complete_job".to_owned()),
      git_ref: None,
    })
    .await;
  let outputs = match evaluate_final_outputs(spec, ctx, &step, conclusion).await {
    Ok(outputs) => outputs,
    Err(error) => {
      // A local job's output expression error still closes this row.
      step
        .issue(AnnotationLevel::Error, &step.mask(&error.to_string()))
        .await;
      step.complete(Conclusion::Failure).await;
      return Err(error);
    },
  };
  let (environment_url, url_failed) = report_environment_url(&step, msg, ctx).await;
  let failed = outputs.failed || url_failed;
  let conclusion = if url_failed && outputs.conclusion != Conclusion::Cancelled {
    Conclusion::Failure
  } else {
    outputs.conclusion
  };
  // The line states what the job reports, so a shutdown is applied here too.
  let conclusion = super::entry::after_shutdown(ctx, conclusion);
  // Upstream's row always carries log lines; reporting the final verdict
  // guarantees this row is never an empty (log-less) result.
  step
    .log(&format!(
      "Job conclusion: {}",
      conclusion.to_report_string()
    ))
    .await;
  step
    .complete(if failed {
      Conclusion::Failure
    } else {
      Conclusion::Success
    })
    .await;
  Ok(JobResult {
    conclusion,
    outputs: outputs.outputs,
    environment_url,
  })
}

/// Evaluate and log the environment URL; returns it and whether it failed.
async fn report_environment_url(
  step: &CompleteStep<'_>,
  msg: &AgentJobRequestMessage,
  ctx: &ExecutionContext,
) -> (Option<String>, bool) {
  let environment = msg.actions_environment.as_ref();
  let result = evaluate_environment_url(environment, ctx);
  if result == EnvironmentUrl::Absent {
    return (None, false);
  }
  step.log("Evaluate and set environment url").await;
  match result {
    EnvironmentUrl::Absent => (None, false),
    EnvironmentUrl::Url(url) => {
      step.log(&format!("Evaluated environment url: {url}")).await;
      (Some(url), false)
    },
    EnvironmentUrl::Secret => {
      let name = environment
        .and_then(|environment| environment.name.as_deref())
        .unwrap_or_default();
      let name = step.mask(name);
      step
        .issue(
          AnnotationLevel::Warning,
          &format!("Skip setting environment url as environment '{name}' may contain secret."),
        )
        .await;
      (None, false)
    },
    EnvironmentUrl::Error(message) => {
      step
        .issue(AnnotationLevel::Error, "Failed to evaluate environment url")
        .await;
      step.issue(AnnotationLevel::Error, &message).await;
      (None, true)
    },
  }
}
