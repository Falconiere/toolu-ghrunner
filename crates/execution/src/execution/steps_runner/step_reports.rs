//! Step lifecycle events: start (with any display-name warning), skip,
//! pre-handler failure, and completion.

use std::collections::HashMap;

use shared::{ActionStep, Conclusion, RunnerError, RunnerEvent};
use tokio::sync::mpsc;

use super::super::context::ExecutionContext;
use super::super::step_display::emit_warning;
use super::step_errors::report_step_failure;

/// A step's reported name and the display-name warning to log once it starts.
pub(super) struct StepName {
  pub(super) name: String,
  pub(super) warning: Option<String>,
}

pub(super) async fn complete_step(
  step: &ActionStep,
  conclusion: Conclusion,
  outputs: HashMap<String, String>,
  events: &mpsc::Sender<RunnerEvent>,
) {
  let _ = events
    .send(RunnerEvent::StepCompleted {
      step_id: step.id.clone(),
      conclusion,
      outputs,
    })
    .await;
}

/// Report failure before handler execution, where continue-on-error cannot apply.
pub(super) async fn record_condition_failure(
  step: &ActionStep,
  name: &StepName,
  step_number: u32,
  ctx: &mut ExecutionContext,
  events: &mpsc::Sender<RunnerEvent>,
  error: &RunnerError,
) {
  start_step(step, name, step_number, events).await;
  if let Some(name) = step.expression_name() {
    ctx.set_step_outcome(name, Conclusion::Failure);
    ctx.set_step_conclusion(name, Conclusion::Failure);
  }
  ctx.record_step_failure();
  report_step_failure(events, &step.id, error).await;
}

pub(super) async fn start_step(
  step: &ActionStep,
  name: &StepName,
  step_number: u32,
  events: &mpsc::Sender<RunnerEvent>,
) {
  let _ = events
    .send(RunnerEvent::StepStarted {
      step_id: step.id.clone(),
      step_name: name.name.clone(),
      step_number,
    })
    .await;
  if let Some(warning) = &name.warning {
    emit_warning(events, &step.id, warning).await;
  }
}

pub(super) async fn report_skipped_step(
  step: &ActionStep,
  name: &StepName,
  step_number: u32,
  ctx: &mut ExecutionContext,
  events: &mpsc::Sender<RunnerEvent>,
) {
  if let Some(name) = step.expression_name() {
    ctx.set_step_outcome(name, Conclusion::Skipped);
    ctx.set_step_conclusion(name, Conclusion::Skipped);
  }
  start_step(step, name, step_number, events).await;
  let _ = events
    .send(RunnerEvent::StepSkipped {
      step_id: step.id.clone(),
      reason: "condition evaluated to false".to_owned(),
    })
    .await;
  complete_step(step, Conclusion::Skipped, HashMap::new(), events).await;
}
