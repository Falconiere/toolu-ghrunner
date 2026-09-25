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
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::actions::manifest::RunsUsing;
use super::context::ExecutionContext;
use super::node_stage::{NodeStage, emit_stage_endgroup, run_node_stage};
use super::step_naming::{PostStep, derive_step_name};
use super::step_timeout::StepBounds;
use super::steps_runner::JobCtx;

#[cfg(test)]
#[path = "tests/post_drain.rs"]
mod tests;

/// Reporting identity and shared cleanup bound for one queued post.
struct PostReport<'a> {
  id: &'a str,
  number: u32,
  cancellation_deadline: Option<Instant>,
}

/// Keep overflow visible when assigning the post's timeline step number.
pub(super) fn post_number(first: u32, index: usize) -> u32 {
  let index = if let Ok(number) = u32::try_from(index) {
    number
  } else {
    tracing::warn!(
      post_index = index,
      "post timeline index overflow; saturating"
    );
    u32::MAX
  };
  if let Some(number) = first.checked_add(index) {
    number
  } else {
    tracing::warn!(
      first_post_number = first,
      post_index = index,
      "post timeline number overflow; saturating"
    );
    u32::MAX
  }
}

/// Drain the post-step queue LIFO and run each post that passes its condition.
pub(super) async fn drain_post_steps(
  posts: &mut super::step_naming::PostStepQueue,
  ctx: &mut ExecutionContext,
  events: &mpsc::Sender<RunnerEvent>,
  job: &JobCtx<'_>,
) -> Conclusion {
  let mut aggregate = Conclusion::Success;
  let mut cancellation_deadline = None;
  for (index, post) in posts.drain_lifo().into_iter().enumerate() {
    if job.cancel.is_cancelled() {
      ctx.record_job_cancelled();
      cancellation_deadline.get_or_insert_with(|| Instant::now() + CANCELLED_POST_GRACE);
    }
    let number = post_number(job.first_post_number, index);
    let report = PostReport {
      id: &post.report_id,
      number,
      cancellation_deadline,
    };
    let result = match run_one_post(&post, &report, ctx, events, job).await {
      Ok(result) => result,
      Err(error) => {
        report_post_error(events, report.id, &error).await;
        Conclusion::Failure
      },
    };
    if result == Conclusion::Failure {
      if aggregate != Conclusion::Cancelled {
        aggregate = Conclusion::Failure;
      }
      ctx.record_step_failure();
    } else if result == Conclusion::Cancelled {
      aggregate = Conclusion::Cancelled;
      ctx.record_job_cancelled();
    }
  }
  aggregate
}

/// Evaluate one post-step's condition and run its `post` entrypoint if it holds.
async fn run_one_post(
  post: &PostStep,
  report: &PostReport<'_>,
  ctx: &mut ExecutionContext,
  events: &mpsc::Sender<RunnerEvent>,
  job: &JobCtx<'_>,
) -> Result<Conclusion, RunnerError> {
  let prior_scope = ctx.scope_path();
  ctx.restore_step_scope(post.scope_path.clone());
  ctx.push_step_env(post.step_env.clone());
  let result = run_scoped_post(post, report, ctx, events, job).await;
  ctx.pop_step_env();
  ctx.restore_step_scope(prior_scope);
  result
}

async fn run_scoped_post(
  post: &PostStep,
  report: &PostReport<'_>,
  ctx: &mut ExecutionContext,
  events: &mpsc::Sender<RunnerEvent>,
  job: &JobCtx<'_>,
) -> Result<Conclusion, RunnerError> {
  let condition = post.effective_condition();
  emit_post_header(
    events,
    report.id,
    &derive_step_name(&post.step),
    report.number,
  )
  .await;

  if !evaluate_post_condition(ctx, condition)? {
    skip_post(
      events,
      report.id,
      format!("post-if '{condition}' evaluated to false"),
    )
    .await;
    return Ok(Conclusion::Skipped);
  }

  let RunsUsing::Node { .. } = post.manifest.runs.using else {
    // Only node actions register a post entrypoint today.
    return Err(RunnerError::ActionManifest(
      "post stage requires a node action".to_owned(),
    ));
  };

  let bounds = post_bounds(post, job, report.cancellation_deadline);
  if bounds.timeout == Some(Duration::ZERO) {
    skip_post(
      events,
      report.id,
      "post cancellation grace elapsed".to_owned(),
    )
    .await;
    return Ok(Conclusion::Skipped);
  }
  let conclusion = run_post_node_stage(post, report, ctx, events, job, &bounds).await?;

  complete_post(events, report.id, conclusion).await;
  Ok(conclusion)
}

fn evaluate_post_condition(ctx: &ExecutionContext, condition: &str) -> Result<bool, RunnerError> {
  let mut eval_ctx = ctx.eval_context();
  // A queued post evaluates after all main steps, against the live job status.
  // Its saved composite scope still supplies inputs, steps, and STATE_*.
  eval_ctx.job_status = ctx.job_status();
  Ok(ctx.evaluate_with(&eval_ctx, condition)?.is_truthy())
}

async fn report_post_error(events: &mpsc::Sender<RunnerEvent>, step_id: &str, error: &RunnerError) {
  if events
    .send(RunnerEvent::Log {
      step_id: step_id.to_owned(),
      line: format!("##[error]post-step failed: {error}"),
      stream: shared::LogStream::Stdout,
    })
    .await
    .is_err()
  {
    tracing::warn!(
      step_id,
      "event receiver closed while reporting post-step error"
    );
  }
  complete_post(events, step_id, Conclusion::Failure).await;
}

async fn skip_post(events: &mpsc::Sender<RunnerEvent>, step_id: &str, reason: String) {
  let _ = events
    .send(RunnerEvent::StepSkipped {
      step_id: step_id.to_owned(),
      reason,
    })
    .await;
  complete_post(events, step_id, Conclusion::Skipped).await;
}

async fn complete_post(events: &mpsc::Sender<RunnerEvent>, step_id: &str, conclusion: Conclusion) {
  if events
    .send(RunnerEvent::StepCompleted {
      step_id: step_id.to_owned(),
      conclusion,
      outputs: std::collections::HashMap::new(),
    })
    .await
    .is_err()
  {
    tracing::warn!(
      step_id,
      ?conclusion,
      "event receiver closed while reporting post-step completion"
    );
  }
}

/// Cleanup grace budget for post-steps draining after a job cancel.
const CANCELLED_POST_GRACE: Duration = Duration::from_secs(5 * 60);

/// Bounds for one post-step run.
///
/// A cancelled job still runs its cleanup posts (matching the upstream
/// runner's cancel-grace behavior): the fired job token would kill the post
/// child the instant it spawned, so a cancelled drain runs each post under a
/// fresh token bounded by the remaining shared [`CANCELLED_POST_GRACE`]
/// deadline (or the step's own tighter `timeout-minutes`). An uncancelled drain keeps the live job token
/// so SIGINT/SIGTERM still interrupts posts normally.
/// If no duration remains, [`run_one_post`] reports the post as skipped without running it.
fn post_bounds(post: &PostStep, job: &JobCtx<'_>, deadline: Option<Instant>) -> StepBounds {
  if !job.cancel.is_cancelled() {
    return StepBounds::new(post.step.timeout_in_minutes, job.cancel.clone());
  }
  let remaining = deadline.map_or(CANCELLED_POST_GRACE, cancel_remaining);
  let step_timeout = super::step_timeout::timeout_duration(post.step.timeout_in_minutes);
  StepBounds {
    timeout: Some(step_timeout.map_or(remaining, |limit| limit.min(remaining))),
    deadline: Some(Instant::now() + step_timeout.map_or(remaining, |limit| limit.min(remaining))),
    cancel: CancellationToken::new(),
  }
}

/// Time left in the one deadline shared by every post after cancellation.
fn cancel_remaining(deadline: Instant) -> Duration {
  deadline.saturating_duration_since(Instant::now())
}

/// Run the `post` node entrypoint in the originating step's scope.
async fn run_post_node_stage(
  post: &PostStep,
  report: &PostReport<'_>,
  ctx: &mut ExecutionContext,
  events: &mpsc::Sender<RunnerEvent>,
  job: &JobCtx<'_>,
  bounds: &StepBounds,
) -> Result<Conclusion, RunnerError> {
  // The post stage runs in the originating step's scope; its outputs are
  // already recorded on `ctx`, and the post `StepCompleted` carries no
  // outputs map (matches the C# runner), so the dispatcher map is dropped.
  let stage = run_node_stage(NodeStage {
    step: &post.step,
    ctx,
    events,
    workspace: job.workspace,
    config: job.config,
    client: job.http,
    action_dir: &post.action_dir,
    manifest: &post.manifest,
    major: post.major,
    bounds,
    stage: "post",
    // Report and upload post logs under their own timeline ID. The original
    // step ID remains the action state/output key inside run_node_stage.
    log_step_id: report.id,
  });
  let (conclusion, _outputs) = if job.cancel.is_cancelled() {
    let remaining = report
      .cancellation_deadline
      .map_or(CANCELLED_POST_GRACE, cancel_remaining);
    match tokio::time::timeout(remaining, stage).await {
      Ok(result) => result?,
      Err(_) => return Ok(Conclusion::Cancelled),
    }
  } else {
    tokio::select! {
      () = job.cancel.cancelled() => return Ok(Conclusion::Cancelled),
      result = stage => result?,
    }
  };
  Ok(conclusion)
}

/// Emit the `Post <action>` group header before a post stage runs.
async fn emit_post_header(
  events: &mpsc::Sender<RunnerEvent>,
  step_id: &str,
  main_name: &str,
  step_number: u32,
) {
  let name = if main_name.is_empty() {
    "Post".to_owned()
  } else {
    format!("Post {main_name}")
  };
  let _ = events
    .send(RunnerEvent::StepStarted {
      step_id: step_id.to_owned(),
      step_name: name.clone(),
      step_number,
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
