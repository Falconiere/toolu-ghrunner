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
) -> Option<(Conclusion, Option<PostStep>)> {
  let uses = build_uses_ref(&step.reference);
  let local = step
    .reference
    .repository_type
    .as_deref()
    .is_some_and(|k| k.eq_ignore_ascii_case("self"));
  let action_ref = parse_action_ref(&uses)
    .ok()
    .filter(|_| !local && !uses.starts_with("docker://"))?;
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
  let (action_dir, manifest) = action_dir.and_then(|d| read_manifest(&d).ok().map(|m| (d, m)))?;
  let RunsUsing::Node { major } = manifest.runs.using else {
    return None;
  };
  manifest.runs.pre.as_ref()?;
  ctx.pre_handled.insert(step.id.clone());
  let name = format!("Pre {}", display_for(step, ctx).initial);
  let step_env = match resolve_step_env(step, ctx, &ctx.eval_context()) {
    Ok(env) => env,
    Err(error) => return Some((failed_row(run, &name, number, &error).await, None)),
  };
  ctx.push_step_env(step_env);
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
  let result = phase_pre_stage(&mut c, &name, number, run).await;
  prior.restore(ctx);
  ctx.pop_step_env();
  Some(result)
}

/// Run the row: skipped when `pre-if` is false, failed when it cannot be
/// evaluated; otherwise the post is registered before the stage runs.
async fn phase_pre_stage(
  c: &mut NodeActionCtx<'_>,
  name: &str,
  number: u32,
  run: &ActionRun<'_>,
) -> (Conclusion, Option<PostStep>) {
  let condition = c.manifest.runs.pre_if.as_deref().unwrap_or("always()");
  match c.ctx.evaluate_expression(condition) {
    Ok(value) if value.is_truthy() => {},
    Ok(_) => {
      let reason = format!("pre-if '{condition}' evaluated to false");
      close_row(run, name, number, Conclusion::Skipped, Some(reason), None).await;
      return (Conclusion::Skipped, None);
    },
    Err(error) => return (failed_row(run, name, number, &error).await, None),
  }
  let post = build_post_step(c);
  if post.is_some() {
    c.ctx.post_registered.insert(c.step.id.clone());
  }
  // The stage reports its own row; a hard error has closed it as failed.
  let conclusion = report_pre_stage(c, number)
    .await
    .unwrap_or(Conclusion::Failure);
  (conclusion, post)
}

async fn failed_row(
  run: &ActionRun<'_>,
  name: &str,
  number: u32,
  error: &RunnerError,
) -> Conclusion {
  let line = format!("##[error]{error}");
  close_row(run, name, number, Conclusion::Failure, None, Some(line)).await;
  Conclusion::Failure
}

/// One whole `Pre` row that runs no stage: started, optionally skipped or
/// logged, completed.
async fn close_row(
  run: &ActionRun<'_>,
  name: &str,
  number: u32,
  conclusion: Conclusion,
  skipped: Option<String>,
  error: Option<String>,
) {
  let id = uuid::Uuid::new_v4().to_string();
  let mut events = vec![RunnerEvent::StepStarted {
    step_id: id.clone(),
    step_name: name.to_owned(),
    step_number: number,
  }];
  if let Some(reason) = skipped {
    events.push(RunnerEvent::StepSkipped {
      step_id: id.clone(),
      reason,
    });
  }
  if let Some(line) = error {
    events.push(RunnerEvent::Log {
      step_id: id.clone(),
      line,
      stream: shared::LogStream::Stdout,
    });
  }
  events.push(RunnerEvent::StepCompleted {
    step_id: id,
    conclusion,
    outputs: std::collections::HashMap::new(),
  });
  for event in events {
    if run.events.send(event).await.is_err() {
      tracing::warn!("pre step event receiver closed");
    }
  }
}
