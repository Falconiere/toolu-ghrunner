//! Upstream pre-job phase (v2.338.2 `JobExtension.InitializeJob`): every
//! top-level repository action's `pre` runs after the setup rows and before
//! the first main step, in step order, and registers its post then. Paired
//! run 42407955591 showed official `Pre` rows ahead of `actions/checkout`.

use shared::{ActionStep, Conclusion, RunnerEvent};
use tokio::sync::mpsc;

use super::{JobCtx, JobState};
use crate::execution::action_exec::ActionRun;
use crate::execution::action_pre::{phase_pre_target, run_phase_pre};
use crate::execution::context::ExecutionContext;
use crate::execution::step_attrs::evaluate_timeout;
use crate::execution::step_timeout::StepBounds;

/// Run each eligible step's `pre` as a numbered row after the setup rows;
/// a failed `pre` fails the job (so `success()` mains are skipped), as upstream.
/// Returns the worst row conclusion, merged into the job's by the caller.
pub(super) async fn run_pre_phase(
  steps: &[ActionStep],
  ctx: &mut ExecutionContext,
  events: &mpsc::Sender<RunnerEvent>,
  job: &JobCtx<'_>,
  job_state: &mut JobState,
) -> Conclusion {
  let mut worst = Conclusion::Success;
  for step in steps.iter().filter(|step| !step.is_run_step()) {
    if job.cancellation.is_forced() {
      return worst;
    }
    let number = 2u32.saturating_add(ctx.pre_job_rows);
    let unbounded = StepBounds::nested(job.cancellation.deadline(), None, job.cancel.clone());
    let mut run = ActionRun {
      events,
      workspace: job.workspace,
      config: job.config,
      bounds: &unbounded,
      http: job.http,
      fetcher: job.fetcher,
      log_step_id: &step.id,
    };
    let Some(target) = phase_pre_target(step, &run).await else {
      continue;
    };
    // Only a confirmed pre evaluates the step's timeout here, silently: the
    // Pre row reports an error itself once started, the main row at main.
    let timeout = evaluate_timeout(step.timeout_in_minutes.as_ref(), ctx)
      .ok()
      .flatten();
    let bounds = StepBounds::nested(job.cancellation.deadline(), timeout, job.cancel.clone());
    run.bounds = &bounds;
    let (conclusion, post) = run_phase_pre(step, ctx, &run, number, target).await;
    if let Some(post) = post {
      job_state.posts.register(post);
    }
    ctx.pre_job_rows = ctx.pre_job_rows.saturating_add(1);
    if conclusion == Conclusion::Failure {
      ctx.record_step_failure();
      if worst == Conclusion::Success {
        worst = Conclusion::Failure;
      }
    } else if conclusion == Conclusion::Cancelled {
      ctx.record_job_cancelled();
      worst = Conclusion::Cancelled;
    }
  }
  worst
}

#[cfg(test)]
#[path = "../tests/pre_phase_guard.rs"]
mod tests;
