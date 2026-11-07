//! Upstream pre-job phase for one step (v2.338.2 `JobExtension.InitializeJob`
//! and `ActionRunner`): a top-level repository Node action's `pre` runs as
//! its own numbered row before any main step and registers the post first.

use shared::{ActionStep, Conclusion, RunnerError, RunnerEvent};

use super::action_exec::{
  ActionRun, NodeActionCtx, build_post_step, build_uses_ref, report_pre_stage,
};
use super::action_support::{read_manifest, resolve_action_dir};
use super::actions::manifest::RunsUsing;
use super::actions::resolver::{ActionRefKind, parse_action_ref};
use super::context::ExecutionContext;
use super::step_display::display_for;
use super::step_env::resolve_step_env;
use super::step_naming::PostStep;

/// Upstream pre-job phase (v2.338.2 `JobExtension`/`ActionRunner`): run a
/// top-level repository Node action's `pre` as its own row `number`, before
/// any main step, registering its post first as upstream does. `None` when
/// the step has no such `pre` (local, image, unresolvable: the main reports).
pub(crate) async fn run_phase_pre(
  step: &ActionStep,
  ctx: &mut ExecutionContext,
  run: &ActionRun<'_>,
  number: u32,
) -> Result<Option<(Conclusion, Option<PostStep>)>, RunnerError> {
  let uses = build_uses_ref(&step.reference);
  let local = step
    .reference
    .repository_type
    .as_deref()
    .is_some_and(|k| k.eq_ignore_ascii_case("self"));
  let Some(action_ref) = parse_action_ref(&uses)
    .ok()
    .filter(|_| !local && !uses.starts_with("docker://"))
  else {
    return Ok(None);
  };
  let action_dir = if action_ref.kind == ActionRefKind::Local {
    action_ref.local_dir(run.workspace)
  } else {
    let cache = run
      .fetcher
      .ensure_action(run.http, &action_ref, &run.config.data_dir)
      .await;
    cache
      .ok()
      .map(|dir| resolve_action_dir(&dir, action_ref.subpath.as_ref()))
  };
  let Some((action_dir, manifest)) =
    action_dir.and_then(|d| read_manifest(&d).ok().map(|m| (d, m)))
  else {
    return Ok(None);
  };
  let RunsUsing::Node { major } = manifest.runs.using else {
    return Ok(None);
  };
  if manifest.runs.pre.is_none() {
    return Ok(None);
  }
  ctx.pre_handled.insert(step.id.clone());
  ctx.push_step_env(resolve_step_env(step, ctx, &ctx.eval_context())?);
  let prior = super::action_metadata::ActionMetadata::capture(ctx);
  super::action_metadata::set_reference(ctx, &step.reference);
  let mut c = NodeActionCtx {
    step,
    ctx,
    events: run.events,
    workspace: run.workspace,
    config: run.config,
    client: run.http,
    action_dir: &action_dir,
    manifest: &manifest,
    major,
    bounds: run.bounds,
    log_step_id: run.log_step_id,
  };
  let result = phase_pre_stage(&mut c, number).await;
  prior.restore(ctx);
  ctx.pop_step_env();
  result.map(Some)
}

async fn phase_pre_stage(
  c: &mut NodeActionCtx<'_>,
  number: u32,
) -> Result<(Conclusion, Option<PostStep>), RunnerError> {
  let condition = c.manifest.runs.pre_if.as_deref().unwrap_or("always()");
  if !c.ctx.evaluate_expression(condition)?.is_truthy() {
    let id = uuid::Uuid::new_v4().to_string();
    let name = format!("Pre {}", display_for(c.step, c.ctx).initial);
    let skipped = [
      RunnerEvent::StepStarted {
        step_id: id.clone(),
        step_name: name,
        step_number: number,
      },
      RunnerEvent::StepSkipped {
        step_id: id.clone(),
        reason: format!("pre-if '{condition}' evaluated to false"),
      },
      RunnerEvent::StepCompleted {
        step_id: id,
        conclusion: Conclusion::Skipped,
        outputs: std::collections::HashMap::new(),
      },
    ];
    for event in skipped {
      let _ = c.events.send(event).await;
    }
    return Ok((Conclusion::Skipped, None));
  }
  let post = build_post_step(c);
  if post.is_some() {
    c.ctx.post_registered.insert(c.step.id.clone());
  }
  Ok((report_pre_stage(c, number).await?, post))
}
