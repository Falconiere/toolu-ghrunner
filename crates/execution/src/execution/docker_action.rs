//! Docker image preparation and pre/main/post lifecycle registration.

use std::collections::HashMap;
use std::sync::Arc;

use shared::{Conclusion, RunnerError, RunnerEvent};

use super::action_exec::ActionOutcome;
use super::action_support::emit_log;
use super::context::ExecutionContext;
use super::docker_stage::{DockerStage, run_docker_stage};
use super::step_display::display_for;
use super::step_naming::PostStep;
use super::step_timeout::{step_timeout_message, with_timeout_message};
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
  if !ctx.post_registered.contains(&s.step.id) {
    register_post(&s, ctx, &image, &inputs);
  }
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

/// The pre-job phase's Docker `pre` (`action_pre`): prepare the image, register
/// the post before the stage as upstream does, then run `pre-entrypoint` as
/// row `number`. The main step later skips both.
pub(super) async fn run_phase_pre(
  s: DockerStage<'_>,
  ctx: &mut ExecutionContext,
  cache_key: &str,
  number: u32,
) -> Result<(Conclusion, Option<PostStep>), RunnerError> {
  let (runtime, image) = prepare(&s, ctx, Some(cache_key)).await?;
  let inputs = super::action_support::build_composite_inputs(s.step, s.manifest, ctx)?;
  let s = DockerStage {
    inputs: Some(&inputs),
    ..s
  };
  let mark = ctx.nested_post_count();
  register_post(&s, ctx, &image, &inputs);
  let post = ctx.take_nested_posts_since(mark).pop();
  if post.is_some() {
    ctx.post_registered.insert(s.step.id.clone());
  }
  let conclusion = run_pre(&s, ctx, &runtime, &image, number)
    .await
    .unwrap_or(Conclusion::Failure);
  Ok((conclusion, post))
}

async fn run_applicable_pre(
  s: &DockerStage<'_>,
  ctx: &mut ExecutionContext,
  runtime: &ActionContainer,
  image: &str,
  remote: bool,
) -> Result<Conclusion, RunnerError> {
  if s.manifest.runs.pre_entrypoint.is_none() || ctx.pre_handled.contains(&s.step.id) {
    return Ok(Conclusion::Success);
  }
  if !remote {
    super::node_stage::skip_local_pre(s.events, s.log_step_id, s.step).await;
    return Ok(Conclusion::Success);
  }
  if !ctx
    .evaluate_expression(s.manifest.runs.pre_if.as_deref().unwrap_or("always()"))?
    .is_truthy()
  {
    return Ok(Conclusion::Success);
  }
  run_pre(s, ctx, runtime, image, 0).await
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
  let image = if let Some(prepared) = super::docker_prepare::image_key(image, cache_key)
    .and_then(|key| ctx.prepared_docker_images.get(&key))
  {
    prepared.clone()
  } else {
    s.bounds
      .resolve_within_bounds(runtime.prepare_image(
        image,
        s.action_dir,
        cache_key,
        &s.bounds.cancel,
      ))
      .await?
  };
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
      nested: Vec::new(),
    });
  }
}

async fn run_pre(
  s: &DockerStage<'_>,
  ctx: &mut ExecutionContext,
  runtime: &ActionContainer,
  image: &str,
  number: u32,
) -> Result<Conclusion, RunnerError> {
  let id = uuid::Uuid::new_v4().to_string();
  let pre_name = format!("Pre {}", display_for(s.step, ctx).initial);
  let message = step_timeout_message(&pre_name, s.bounds.own_timeout);
  emit_pre_event(
    s.events,
    RunnerEvent::StepStarted {
      step_id: id.clone(),
      step_name: pre_name,
      step_number: number,
    },
  )
  .await;
  if number > 0 {
    // A pre-job-phase row reports its own timeout error, as upstream's runner does.
    let token = s.step.timeout_in_minutes.as_ref();
    super::step_attrs::resolve_timeout(&id, token, ctx, s.events).await;
  }
  let docker = super::actions::manifest::RunsUsing::Docker;
  super::step_metadata::emit_action_metadata(s.events, &id, &s.step.reference, &docker).await;
  let pre_bounds = s.bounds.restarted();
  let pre = DockerStage {
    stage: "pre",
    log_step_id: &id,
    bounds: &pre_bounds,
    ..*s
  };
  // A job whose declared container never started runs no stage at all.
  let result = if ctx.job_container_missing {
    emit_log(s.events, &id, "##[error]the job container is not running").await;
    Ok((Conclusion::Failure, HashMap::new()))
  } else {
    let stage = Box::pin(run_docker_stage(&pre, ctx, runtime, image));
    with_timeout_message(message, stage).await
  };
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
