//! Download and execute GitHub Actions (`uses:` steps).

use std::path::Path;

use shared::{ActionStep, Conclusion, RunnerConfig, RunnerError, RunnerEvent};
use tokio::sync::mpsc;

use super::action_support::{
  build_composite_inputs, emit_action_header, emit_log, read_manifest, resolve_action_dir,
};
use super::actions::downloader::{action_cache_dir, download_and_extract_action, is_action_cached};
use super::actions::manifest::{ActionDefinition, RunsUsing};
use super::actions::resolver::{ActionRefKind, parse_action_ref};
use super::composite_exec::{CompositeParams, execute_composite_action};
use super::context::ExecutionContext;
use super::depth_tracker::DepthTracker;
use super::node_stage::{NodeStage, emit_stage_endgroup, run_node_stage};
use super::step_naming::PostStep;
use super::step_timeout::StepBounds;

/// Resolved action ready for execution.
struct ResolvedStep {
  client: reqwest::Client,
  action_dir: std::path::PathBuf,
  manifest: super::actions::manifest::ActionDefinition,
}

/// Immutable per-step environment shared across an action's dispatch path:
/// the event sink, the workspace root, the runner config, and the step's
/// timeout / cancellation bounds (applied to every node child it spawns).
struct ActionEnv<'a> {
  events: &'a mpsc::Sender<RunnerEvent>,
  workspace: &'a Path,
  config: &'a RunnerConfig,
  bounds: &'a StepBounds,
}

/// Outcome of running an action step's `pre`+`main` stages.
///
/// `post` is the post-step to register into the job's LIFO queue, if the
/// action defines a `post` entrypoint. `outputs` carries the `main` stage's
/// stdout `::set-output::` values so they reach `StepCompleted.outputs`.
pub struct ActionOutcome {
  pub conclusion: Conclusion,
  pub post: Option<PostStep>,
  pub outputs: std::collections::HashMap<String, String>,
}

/// Per-step inputs for running an action: the workspace root, runner config,
/// timeout / cancellation bounds, and the event sink. Bundled so the entry
/// point stays under the argument ceiling.
pub(crate) struct ActionRun<'a> {
  pub(crate) events: &'a mpsc::Sender<RunnerEvent>,
  pub(crate) workspace: &'a Path,
  pub(crate) config: &'a RunnerConfig,
  pub(crate) bounds: &'a StepBounds,
}

/// Execute an action step end-to-end: resolve -> download -> parse manifest ->
/// run `pre` (if any, gated by `pre-if`) -> run `main`. Any `post` entrypoint
/// is returned for the caller to register and drain LIFO at job end.
///
/// # Errors
///
/// Returns `RunnerError` on download, manifest parse, or execution failure.
pub(crate) async fn execute_action(
  step: &ActionStep,
  ctx: &mut ExecutionContext,
  run: &ActionRun<'_>,
  depth: &mut DepthTracker,
) -> Result<ActionOutcome, RunnerError> {
  let resolved = resolve_action(step, run.workspace, run.config, run.events).await?;
  let env = ActionEnv {
    events: run.events,
    workspace: run.workspace,
    config: run.config,
    bounds: run.bounds,
  };
  dispatch_action(step, ctx, &env, &resolved, depth).await
}

/// Resolve an action step to its on-disk directory + manifest.
///
/// Remote actions are downloaded+cached; local `./path` actions resolve to a
/// directory under `workspace` (the checked-out repo) with no network access.
///
/// # Errors
///
/// Returns `RunnerError` on resolution, download, or manifest parse failure.
async fn resolve_action(
  step: &ActionStep,
  workspace: &Path,
  config: &RunnerConfig,
  events: &mpsc::Sender<RunnerEvent>,
) -> Result<ResolvedStep, RunnerError> {
  let uses = step
    .reference
    .name
    .as_deref()
    .or(step.reference.image.as_deref())
    .unwrap_or("");
  let git_ref = step.reference.git_ref.as_deref().unwrap_or("");
  let uses_full = if git_ref.is_empty() {
    uses.to_owned()
  } else {
    format!("{uses}@{git_ref}")
  };

  let action_ref = parse_action_ref(&uses_full)?;

  // Build one timeout-bounded client per resolution and reuse it for the
  // tarball download and any node-runtime download — a fresh `Client::new()`
  // per call has no request timeout, so a hung connection would block the
  // step forever. Downloads can be large, so the timeout is generous.
  let client = action_client()?;

  if action_ref.kind == ActionRefKind::Local {
    return resolve_local_action(step, &action_ref, &uses_full, workspace, events, client).await;
  }

  resolve_remote_action(step, &action_ref, &uses_full, config, events, client).await
}

/// Build the per-resolution HTTP client with a generous request timeout
/// (downloads can be large). Propagates the builder error rather than
/// unwrapping.
fn action_client() -> Result<reqwest::Client, RunnerError> {
  reqwest::Client::builder()
    .timeout(std::time::Duration::from_secs(120))
    .build()
    .map_err(|e| RunnerError::ActionResolution(format!("build HTTP client: {e}")))
}

/// Resolve a local `./path` action to a directory under `workspace`.
async fn resolve_local_action(
  step: &ActionStep,
  action_ref: &super::actions::resolver::ActionRef,
  uses_full: &str,
  workspace: &Path,
  events: &mpsc::Sender<RunnerEvent>,
  client: reqwest::Client,
) -> Result<ResolvedStep, RunnerError> {
  let action_dir = action_ref.local_dir(workspace).ok_or_else(|| {
    RunnerError::ActionResolution(format!("invalid local action ref '{uses_full}'"))
  })?;
  let manifest = read_manifest(&action_dir)?;
  emit_action_header(step, uses_full, events).await;

  Ok(ResolvedStep {
    client,
    action_dir,
    manifest,
  })
}

/// Resolve a remote `{owner}/{repo}@{ref}` action, downloading on cache miss.
async fn resolve_remote_action(
  step: &ActionStep,
  action_ref: &super::actions::resolver::ActionRef,
  uses_full: &str,
  config: &RunnerConfig,
  events: &mpsc::Sender<RunnerEvent>,
  client: reqwest::Client,
) -> Result<ResolvedStep, RunnerError> {
  let cache_key = action_ref.cache_key();
  let cache_dir = action_cache_dir(&config.data_dir, &cache_key);

  if !is_action_cached(&cache_dir) {
    let tarball_url = action_ref.tarball_url("https://api.github.com");
    emit_log(events, &step.id, &format!("Downloading {uses_full}...")).await;
    download_and_extract_action(&client, &tarball_url, None, &cache_dir).await?;
  }

  let action_dir = resolve_action_dir(&cache_dir, &action_ref.subpath);
  let manifest = read_manifest(&action_dir)?;
  emit_action_header(step, uses_full, events).await;

  Ok(ResolvedStep {
    client,
    action_dir,
    manifest,
  })
}

async fn dispatch_action(
  step: &ActionStep,
  ctx: &mut ExecutionContext,
  env: &ActionEnv<'_>,
  resolved: &ResolvedStep,
  depth: &mut DepthTracker,
) -> Result<ActionOutcome, RunnerError> {
  let ResolvedStep {
    client,
    action_dir,
    manifest,
  } = resolved;

  match manifest.runs.using {
    RunsUsing::Node { major } => {
      let node_ctx = NodeActionCtx {
        step,
        ctx,
        events: env.events,
        workspace: env.workspace,
        config: env.config,
        client,
        action_dir,
        manifest,
        major,
        bounds: env.bounds,
      };
      run_node_action(node_ctx).await
    },
    RunsUsing::Composite => {
      let conclusion = run_composite_action(step, ctx, env, resolved, depth).await?;
      Ok(ActionOutcome {
        conclusion,
        post: None,
        outputs: std::collections::HashMap::new(),
      })
    },
    RunsUsing::Docker => {
      // Fail the step, not the whole job (an `Err` would abort the step loop).
      emit_log(env.events, &step.id, "  (docker actions not yet supported)").await;
      Ok(ActionOutcome {
        conclusion: Conclusion::Failure,
        post: None,
        outputs: std::collections::HashMap::new(),
      })
    },
  }
}

/// RAII guard that exits the composite-action depth level on drop.
///
/// A bare `depth.exit()` after the call is skipped if `run_composite_inner`
/// panics; dropping this guard during unwinding still exits the level, so a
/// panic cannot leak a depth count.
struct DepthExitGuard<'a>(&'a mut DepthTracker);

impl Drop for DepthExitGuard<'_> {
  fn drop(&mut self) {
    self.0.exit();
  }
}

/// Run a composite action and propagate its env/path changes to the parent.
///
/// Enters the depth tracker for the duration so nested `uses:` recursion is
/// bounded; the level is always exited via [`DepthExitGuard`] — even on error
/// or panic.
async fn run_composite_action(
  step: &ActionStep,
  ctx: &mut ExecutionContext,
  env: &ActionEnv<'_>,
  resolved: &ResolvedStep,
  depth: &mut DepthTracker,
) -> Result<Conclusion, RunnerError> {
  depth.enter()?;
  let guard = DepthExitGuard(depth);
  run_composite_inner(step, ctx, env, resolved, &mut *guard.0).await
}

/// Body of `run_composite_action`, separated so the caller's [`DepthExitGuard`]
/// always exits the depth tracker regardless of the result.
async fn run_composite_inner(
  step: &ActionStep,
  ctx: &mut ExecutionContext,
  env: &ActionEnv<'_>,
  resolved: &ResolvedStep,
  depth: &mut DepthTracker,
) -> Result<Conclusion, RunnerError> {
  let step_inputs = build_composite_inputs(step, &resolved.manifest);
  emit_log(env.events, &step.id, "##[endgroup]").await;
  let params = CompositeParams {
    manifest: &resolved.manifest,
    step_inputs: &step_inputs,
    events: env.events,
    workspace: env.workspace,
    config: env.config,
    parent_step_id: &step.id,
    action_dir: &resolved.action_dir,
    cancel: &env.bounds.cancel,
  };
  let result = execute_composite_action(&params, ctx, depth).await?;
  for (k, v) in &result.env_additions {
    ctx.set_env(k, v);
  }
  for p in &result.path_additions {
    ctx.prepend_path(p);
  }
  Ok(result.conclusion)
}

/// Inputs for running a Node.js action's `pre`/`main` stages.
struct NodeActionCtx<'a> {
  step: &'a ActionStep,
  ctx: &'a mut ExecutionContext,
  events: &'a mpsc::Sender<RunnerEvent>,
  workspace: &'a Path,
  config: &'a RunnerConfig,
  client: &'a reqwest::Client,
  action_dir: &'a Path,
  manifest: &'a ActionDefinition,
  major: u8,
  bounds: &'a StepBounds,
}

/// Run a Node.js action: `pre` (if defined and `pre-if` holds) then `main`,
/// dispatching each stage's stdout workflow commands onto the live context.
/// Returns the `main` conclusion plus the `post` registration (if any).
async fn run_node_action(mut c: NodeActionCtx<'_>) -> Result<ActionOutcome, RunnerError> {
  run_node_pre_if_present(&mut c).await?;

  emit_stage_endgroup(c.events, &c.step.id).await;
  // The `main` stage's stdout `::set-output::` values are surfaced on the
  // `StepCompleted` event (consistent with `ctx`).
  let (conclusion, outputs) = run_node_stage(c.stage("main")).await?;

  let post = build_post_step(&c);
  Ok(ActionOutcome {
    conclusion,
    post,
    outputs,
  })
}

/// Run the action's `pre` entrypoint when present and `pre-if` evaluates true.
///
/// The built-in default for an action's `pre-if` is `always()` (the pre step
/// runs unconditionally unless an explicit `pre-if` is given).
async fn run_node_pre_if_present(c: &mut NodeActionCtx<'_>) -> Result<(), RunnerError> {
  if c.manifest.runs.pre.is_none() {
    return Ok(());
  }
  let condition = c.manifest.runs.pre_if.as_deref().unwrap_or("always()");
  if !c.ctx.evaluate_expression(condition)?.is_truthy() {
    return Ok(());
  }

  emit_log(c.events, &c.step.id, "##[group]Pre Run").await;
  emit_stage_endgroup(c.events, &c.step.id).await;
  // A `pre` stage's outputs are recorded on `ctx` but not surfaced on the
  // step's `StepCompleted` (only `main` outputs are), so drop the map.
  let (_conclusion, _outputs) = run_node_stage(c.stage("pre")).await?;
  Ok(())
}

/// Build the post-step registration for a node action that defines `runs.post`.
fn build_post_step(c: &NodeActionCtx<'_>) -> Option<PostStep> {
  c.manifest.runs.post.as_ref()?;
  Some(PostStep {
    step: c.step.clone(),
    action_name: c.manifest.name.clone(),
    action_dir: c.action_dir.to_path_buf(),
    manifest: c.manifest.clone(),
    major: c.major,
    condition: c.manifest.runs.post_if.clone(),
  })
}

impl<'a> NodeActionCtx<'a> {
  /// Build a `NodeStage` for `stage`, reborrowing the live context mutably.
  fn stage<'s>(&'s mut self, stage: &'s str) -> NodeStage<'s> {
    NodeStage {
      step: self.step,
      ctx: &mut *self.ctx,
      events: self.events,
      workspace: self.workspace,
      config: self.config,
      client: self.client,
      action_dir: self.action_dir,
      manifest: self.manifest,
      major: self.major,
      bounds: self.bounds,
      stage,
    }
  }
}
