//! Upstream pre-job phase (v2.338.2 `JobExtension.InitializeJob`): every
//! top-level repository action's `pre` runs after the setup rows and before
//! the first main step, in step order, and registers its post then. Paired
//! run 42407955591 showed official `Pre` rows ahead of `actions/checkout`.

use shared::{ActionStep, Conclusion, RunnerEvent};
use tokio::sync::mpsc;

use super::{JobCtx, JobState};
use crate::execution::action_exec::ActionRun;
use crate::execution::action_pre::run_phase_pre;
use crate::execution::context::ExecutionContext;
use crate::execution::step_attrs::resolve_timeout;
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
    let number = 2u32.saturating_add(ctx.docker_setup_count);
    let timeout = resolve_timeout(&step.id, step.timeout_in_minutes.as_ref(), ctx, events).await;
    let bounds = StepBounds::nested(job.cancellation.deadline(), timeout, job.cancel.clone());
    let run = ActionRun {
      events,
      workspace: job.workspace,
      config: job.config,
      bounds: &bounds,
      http: job.http,
      fetcher: job.fetcher,
      log_step_id: &step.id,
    };
    let Some((conclusion, post)) = run_phase_pre(step, ctx, &run, number).await else {
      continue;
    };
    if let Some(post) = post {
      job_state.posts.register(post);
    }
    ctx.docker_setup_count = ctx.docker_setup_count.saturating_add(1);
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
