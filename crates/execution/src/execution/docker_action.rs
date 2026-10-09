//! Docker image preparation and pre/main/post lifecycle registration.

use std::collections::HashMap;
use std::sync::Arc;

use shared::{Conclusion, RunnerError, RunnerEvent};

use super::action_exec::ActionOutcome;
use super::action_support::emit_log;
use super::context::ExecutionContext;
use super::docker_stage::{DockerStage, run_docker_stage};
use super::step_naming::PostStep;
use crate::docker::action_container::ActionContainer;

/// Prepare one image and run its applicable pre/main stages on Linux.
pub(super) async fn run_docker_action(
  s: DockerStage<'_>,
  ctx: &mut ExecutionContext,
  cache_key: Option<&str>,
) -> Result<ActionOutcome, RunnerError> {
  if !cfg!(target_os = "linux") {
    return Err(RunnerError::Docker(format!(
      "Container actions are only supported on Linux (current host: {})",
      shared::platform::runner_os()
    )));
  }
  let (runtime, image) = prepare(&s, ctx, cache_key).await?;
  emit_log(
    s.events,
    s.log_step_id,
    &format!("Docker action image: {image}"),
  )
  .await;
  let inputs = super::action_support::build_composite_inputs(s.step, s.manifest, ctx)?;
  let s = DockerStage {
    inputs: Some(&inputs),
    ..s
  };
  register_post(&s, ctx, &image, &inputs);
  let pre = run_applicable_pre(&s, ctx, &runtime, &image, cache_key.is_some()).await?;
  if pre != Conclusion::Success {
    return Ok(ActionOutcome {
      conclusion: pre,
      post: None,
      outputs: HashMap::new(),
    });
  }
  emit_log(s.events, s.log_step_id, "##[endgroup]").await;
  let main_bounds = s.bounds.restarted();
  let s = DockerStage {
    bounds: &main_bounds,
    ..s
  };
  let (conclusion, outputs) = run_docker_stage(&s, ctx, &runtime, &image).await?;
  Ok(ActionOutcome {
    conclusion,
    post: None,
    outputs,
  })
}

async fn run_applicable_pre(
  s: &DockerStage<'_>,
  ctx: &mut ExecutionContext,
  runtime: &ActionContainer,
  image: &str,
  remote: bool,
) -> Result<Conclusion, RunnerError> {
  if s.manifest.runs.pre_entrypoint.is_none() {
    return Ok(Conclusion::Success);
  }
  if !remote {
    emit_log(
      s.events,
      s.log_step_id,
      "##[warning]Pre execution is not supported for local actions.",
    )
    .await;
    return Ok(Conclusion::Success);
  }
  if !ctx
    .evaluate_expression(s.manifest.runs.pre_if.as_deref().unwrap_or("always()"))?
    .is_truthy()
  {
    return Ok(Conclusion::Success);
  }
  run_pre(s, ctx, runtime, image).await
}

async fn prepare(
  s: &DockerStage<'_>,
  ctx: &ExecutionContext,
  cache_key: Option<&str>,
) -> Result<(ActionContainer, String), RunnerError> {
  let image = s
    .manifest
    .runs
    .image
    .as_deref()
    .filter(|v| !v.is_empty())
    .ok_or_else(|| RunnerError::ActionManifest("Docker action requires runs.image".to_owned()))?;
  let runtime = s
    .bounds
    .resolve_within_bounds(ActionContainer::connect(Arc::clone(ctx.masker())))
    .await?;
  let image = s
    .bounds
    .resolve_within_bounds(runtime.prepare_image(image, s.action_dir, cache_key, &s.bounds.cancel))
    .await?;
  Ok((runtime, image))
}

fn register_post(
  s: &DockerStage<'_>,
  ctx: &mut ExecutionContext,
  image: &str,
  inputs: &HashMap<String, String>,
) {
  // The caller drains this queue even when pre or main returns a hard error.
  if s.manifest.runs.post_entrypoint.is_some() {
    ctx.register_nested_post(PostStep {
      step: s.step.clone(),
      action_name: ctx.github_context("action").unwrap_or_default().to_owned(),
      step_env: ctx.snapshot_step_env(),
      report_id: uuid::Uuid::new_v4().to_string(),
      scope_path: ctx.scope_path(),
      action_dir: s.action_dir.to_path_buf(),
      manifest: s.manifest.clone(),
      major: 0,
      docker_image: Some(image.to_owned()),
      docker_inputs: Some(inputs.clone()),
      condition: s.manifest.runs.post_if.clone(),
    });
  }
}

async fn run_pre(
  s: &DockerStage<'_>,
  ctx: &mut ExecutionContext,
  runtime: &ActionContainer,
  image: &str,
) -> Result<Conclusion, RunnerError> {
  let id = uuid::Uuid::new_v4().to_string();
  emit_pre_event(
    s.events,
    RunnerEvent::StepStarted {
      step_id: id.clone(),
      step_name: format!("Pre {}", s.manifest.name),
      step_number: 0,
    },
  )
  .await;
  let pre_bounds = s.bounds.restarted();
  let pre = DockerStage {
    stage: "pre",
    log_step_id: &id,
    bounds: &pre_bounds,
    ..*s
  };
  let result = run_docker_stage(&pre, ctx, runtime, image).await;
  let mut conclusion = match &result {
    Ok((conclusion, _)) => *conclusion,
    Err(RunnerError::Cancelled) => Conclusion::Cancelled,
    Err(_) => Conclusion::Failure,
  };
  let token = s.step.continue_on_error.as_ref();
  if result.is_ok()
    && conclusion == Conclusion::Failure
    && super::step_attrs::resolve_continue_on_error(&id, token, ctx, s.events).await
  {
    conclusion = Conclusion::Success;
  }
  emit_pre_event(
    s.events,
    RunnerEvent::StepCompleted {
      step_id: id,
      conclusion,
      outputs: HashMap::new(),
    },
  )
  .await;
  result.map(|_| conclusion)
}

async fn emit_pre_event(events: &tokio::sync::mpsc::Sender<RunnerEvent>, event: RunnerEvent) {
  if let Err(error) = events.send(event).await {
    // The receiver was dropped or explicitly closed. A synthetic completion
    // cannot enter that channel either; preserve the action cleanup path.
    tracing::warn!(%error, "Docker pre-stage event channel closed; continuing cleanup");
  }
}
