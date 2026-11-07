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
/// A step whose `pre` belongs in the pre-job phase: its action directory and
/// manifest, and how it runs. Resolved quietly; `None` leaves the step alone.
pub(crate) struct PreTarget {
  action_dir: std::path::PathBuf,
  manifest: super::actions::manifest::ActionDefinition,
  kind: PreKind,
}

/// A Node `pre` (with its major) or a Docker `pre-entrypoint` (with the image
/// cache key its prepared image is stored under).
enum PreKind {
  Node(u8),
  Docker(String),
}

/// The step's pre-job-phase target, if it is a repository Node action with a
/// `pre` or, on Linux, a remote Docker action with a `pre-entrypoint`.
/// Evaluates nothing of the step, so a step without one is untouched.
pub(crate) async fn phase_pre_target(step: &ActionStep, run: &ActionRun<'_>) -> Option<PreTarget> {
  let uses = build_uses_ref(&step.reference);
  let local = step
    .reference
    .repository_type
    .as_deref()
    .is_some_and(|k| k.eq_ignore_ascii_case("self"));
  let action_ref = parse_action_ref(&uses)
    .ok()
    .filter(|_| !local && !uses.starts_with("docker://"))?;
  let remote = action_ref.kind != ActionRefKind::Local;
  let action_dir = if remote {
    let cache = run
      .fetcher
      .ensure_action(run.http, &action_ref, &run.config.data_dir)
      .await;
    cache
      .ok()
      .map(|dir| resolve_action_dir(&dir, action_ref.subpath.as_ref()))
  } else {
    action_ref.local_dir(run.workspace)
  };
  let (action_dir, manifest) = action_dir.and_then(|d| read_manifest(&d).ok().map(|m| (d, m)))?;
  let kind = match manifest.runs.using {
    RunsUsing::Node { major } if manifest.runs.pre.is_some() => PreKind::Node(major),
    // Container actions run only on Linux (the main step reports the rest).
    RunsUsing::Docker
      if remote && cfg!(target_os = "linux") && manifest.runs.pre_entrypoint.is_some() =>
    {
      PreKind::Docker(action_dir.to_string_lossy().into_owned())
    },
    RunsUsing::Node { .. } | RunsUsing::Docker | RunsUsing::Composite => return None,
  };
  Some(PreTarget {
    action_dir,
    manifest,
    kind,
  })
}

/// Run `target`'s `pre` as row `number` (see [`phase_pre_target`]): skipped
/// when `pre-if` is false, failed when the step env or `pre-if` cannot be
/// evaluated; otherwise the post is registered before the stage runs.
pub(crate) async fn run_phase_pre(
  step: &ActionStep,
  ctx: &mut ExecutionContext,
  run: &ActionRun<'_>,
  number: u32,
  target: PreTarget,
) -> (Conclusion, Option<PostStep>) {
  ctx.pre_handled.insert(step.id.clone());
  let name = format!("Pre {}", display_for(step, ctx).initial);
  let step_env = match resolve_step_env(step, ctx, &ctx.eval_context()) {
    Ok(env) => env,
    Err(error) => return (failed_row(run, &name, number, &error).await, None),
  };
  ctx.push_step_env(step_env);
  let prior = super::action_metadata::ActionMetadata::capture(ctx);
  super::action_metadata::set_reference(ctx, &step.reference);
  let condition = target.manifest.runs.pre_if.as_deref().unwrap_or("always()");
  let result = match ctx.evaluate_expression(condition) {
    Ok(value) if value.is_truthy() => phase_pre_stage(step, ctx, run, number, &target).await,
    Ok(_) => {
      let reason = format!("pre-if '{condition}' evaluated to false");
      close_row(run, &name, number, Conclusion::Skipped, Some(reason), None).await;
      (Conclusion::Skipped, None)
    },
    Err(error) => (failed_row(run, &name, number, &error).await, None),
  };
  prior.restore(ctx);
  ctx.pop_step_env();
  result
}

/// The stage itself, which reports its own row (a hard error closes it as failed).
async fn phase_pre_stage(
  step: &ActionStep,
  ctx: &mut ExecutionContext,
  run: &ActionRun<'_>,
  number: u32,
  target: &PreTarget,
) -> (Conclusion, Option<PostStep>) {
  match &target.kind {
    PreKind::Docker(cache_key) => {
      let stage = super::docker_stage::DockerStage {
        step,
        events: run.events,
        workspace: run.workspace,
        config: run.config,
        action_dir: &target.action_dir,
        manifest: &target.manifest,
        bounds: run.bounds,
        stage: "pre",
        log_step_id: run.log_step_id,
        inputs: None,
      };
      match super::docker_action::run_phase_pre(stage, ctx, cache_key, number).await {
        Ok(result) => result,
        Err(error) => {
          let name = format!("Pre {}", display_for(step, ctx).initial);
          (failed_row(run, &name, number, &error).await, None)
        },
      }
    },
    PreKind::Node(major) => {
      let mut c = NodeActionCtx {
        step,
        ctx,
        events: run.events,
        workspace: run.workspace,
        config: run.config,
        client: run.http,
        action_dir: &target.action_dir,
        manifest: &target.manifest,
        major: *major,
        bounds: run.bounds,
        log_step_id: run.log_step_id,
      };
      let post = build_post_step(&c);
      if post.is_some() {
        c.ctx.post_registered.insert(step.id.clone());
      }
      let conclusion = report_pre_stage(&mut c, number)
        .await
        .unwrap_or(Conclusion::Failure);
      (conclusion, post)
    },
  }
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
