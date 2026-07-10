//! Drains the job's `PostStepQueue` LIFO and runs each action's `post` stage.
//!
//! Post steps are registered as their actions' `main` stages run, then drained
//! in reverse order *after* all main steps — including when a prior step failed
//! (so cleanup runs on failure). Each post-step's `post-if` (default
//! `always()`) is evaluated against the live job/steps status: `always()` runs
//! even on failure, while `success()`/`failure()` honor the job status. The
//! post stage runs in the originating step's scope, so it reads the `STATE_*`
//! that `main` saved.

use shared::{Conclusion, RunnerError, RunnerEvent};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::actions::manifest::RunsUsing;
use super::context::ExecutionContext;
use super::node_stage::{NodeStage, emit_stage_endgroup, run_node_stage};
use super::step_naming::PostStep;
use super::step_timeout::StepBounds;
use super::steps_runner::JobCtx;

/// Drain the post-step queue LIFO and run each post that passes its condition.
///
/// # Errors
///
/// Returns `RunnerError` if a post stage fails to spawn/await its entrypoint.
pub(super) async fn drain_post_steps(
  posts: &mut super::step_naming::PostStepQueue,
  ctx: &mut ExecutionContext,
  events: &mpsc::Sender<RunnerEvent>,
  job: &JobCtx<'_>,
) -> Result<(), RunnerError> {
  let drained = posts.drain_lifo();
  if drained.is_empty() {
    return Ok(());
  }
  // A request timeout so a hung node-runtime download can't block post-drain
  // (and thus the whole job) forever.
  let client = reqwest::Client::builder()
    .timeout(std::time::Duration::from_secs(120))
    .build()
    .map_err(|e| RunnerError::NodeHandler(format!("build HTTP client: {e}")))?;
  for post in drained {
    run_one_post(&post, ctx, events, job, &client).await?;
  }
  Ok(())
}

/// Evaluate one post-step's condition and run its `post` entrypoint if it holds.
async fn run_one_post(
  post: &PostStep,
  ctx: &mut ExecutionContext,
  events: &mpsc::Sender<RunnerEvent>,
  job: &JobCtx<'_>,
  client: &reqwest::Client,
) -> Result<(), RunnerError> {
  let step_id = post.step.id.clone();
  let condition = post.effective_condition();

  if !ctx.evaluate_expression(condition)?.is_truthy() {
    let _ = events
      .send(RunnerEvent::StepSkipped {
        step_id,
        reason: format!("post-if '{condition}' evaluated to false"),
      })
      .await;
    return Ok(());
  }

  let RunsUsing::Node { .. } = post.manifest.runs.using else {
    // Only node actions register a post entrypoint today.
    return Ok(());
  };

  emit_post_header(events, &step_id, &post.action_name).await;
  let bounds = post_bounds(post, job);
  let conclusion = run_post_node_stage(post, ctx, events, job, client, &bounds).await?;

  ctx.set_step_outcome(&step_id, conclusion);
  ctx.set_step_conclusion(&step_id, conclusion);
  if conclusion == Conclusion::Failure {
    ctx.record_step_failure();
  }
  let _ = events
    .send(RunnerEvent::StepCompleted {
      step_id,
      conclusion,
      outputs: std::collections::HashMap::new(),
    })
    .await;
  Ok(())
}

/// Cleanup grace budget for post-steps draining after a job cancel.
const CANCELLED_POST_GRACE_MINUTES: u32 = 5;

/// Bounds for one post-step run.
///
/// A cancelled job still runs its cleanup posts (matching the upstream
/// runner's cancel-grace behavior): the fired job token would kill the post
/// child the instant it spawned, so a cancelled drain runs each post under a
/// FRESH token bounded by [`CANCELLED_POST_GRACE_MINUTES`] (or the step's own
/// tighter `timeout-minutes`). An uncancelled drain keeps the live job token
/// so SIGINT/SIGTERM still interrupts posts normally.
fn post_bounds(post: &PostStep, job: &JobCtx<'_>) -> StepBounds {
  if !job.cancel.is_cancelled() {
    return StepBounds::new(post.step.timeout_in_minutes, job.cancel.clone());
  }
  let grace = post
    .step
    .timeout_in_minutes
    .filter(|&t| t > 0)
    .map_or(CANCELLED_POST_GRACE_MINUTES, |t| {
      t.min(CANCELLED_POST_GRACE_MINUTES)
    });
  StepBounds::new(Some(grace), CancellationToken::new())
}

/// Run the `post` node entrypoint in the originating step's scope.
async fn run_post_node_stage(
  post: &PostStep,
  ctx: &mut ExecutionContext,
  events: &mpsc::Sender<RunnerEvent>,
  job: &JobCtx<'_>,
  client: &reqwest::Client,
  bounds: &StepBounds,
) -> Result<Conclusion, RunnerError> {
  // The post stage runs in the originating step's scope; its outputs are
  // already recorded on `ctx`, and the post `StepCompleted` carries no
  // outputs map (matches the C# runner), so the dispatcher map is dropped.
  let (conclusion, _outputs) = run_node_stage(NodeStage {
    step: &post.step,
    ctx,
    events,
    workspace: job.workspace,
    config: job.config,
    client,
    action_dir: &post.action_dir,
    manifest: &post.manifest,
    major: post.major,
    bounds,
    stage: "post",
  })
  .await?;
  Ok(conclusion)
}

/// Emit the `Post <action>` group header before a post stage runs.
async fn emit_post_header(events: &mpsc::Sender<RunnerEvent>, step_id: &str, action_name: &str) {
  let name = if action_name.is_empty() {
    "Post".to_owned()
  } else {
    format!("Post {action_name}")
  };
  let _ = events
    .send(RunnerEvent::StepStarted {
      step_id: step_id.to_owned(),
      step_name: name.clone(),
      step_number: 0,
    })
    .await;
  let _ = events
    .send(RunnerEvent::Log {
      step_id: step_id.to_owned(),
      line: format!("##[group]{name}"),
      stream: shared::LogStream::Stdout,
    })
    .await;
  emit_stage_endgroup(events, step_id).await;
}
