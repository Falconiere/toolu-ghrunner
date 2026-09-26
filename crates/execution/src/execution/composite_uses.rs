//! Nested `uses:` step runner for composite actions.
//!
//! A composite action may invoke other actions (`uses: ./local` or
//! `uses: owner/repo@ref`) as steps. This module builds a synthetic
//! [`ActionStep`] from a composite step's fields and dispatches it through the
//! same action-execution path as a top-level `uses:` step, recursively (the
//! nested action may itself be local/remote/composite). Recursion is bounded by
//! the shared [`DepthTracker`].

use std::collections::HashMap;
use std::path::Path;

use shared::{
  ActionStep, ActionStepDefinitionReference, Conclusion, DictEntry, RunnerConfig, RunnerError,
  RunnerEvent, TemplateToken,
};
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::actions::manifest::CompositeStep;
use super::actions::prefetch::ActionFetcher;
use super::composite_expr::{CompositeField, composite_eval_context, interpolate_composite_expr};
use super::context::ExecutionContext;
use super::depth_tracker::DepthTracker;
use super::step_timeout::StepBounds;

/// Everything a nested `uses:` step needs to resolve + run.
pub struct NestedUsesParams<'a> {
  /// The composite step declaring `uses:`.
  pub step: &'a CompositeStep,
  /// Index of the step within the composite (for synthesizing an id).
  pub idx: usize,
  /// The composite's resolved inputs (for `${{ inputs.X }}` in `with:`).
  pub inputs: &'a HashMap<String, String>,
  /// Live execution context (env, secrets, masking).
  pub ctx: &'a mut ExecutionContext,
  /// Event sink.
  pub events: &'a mpsc::Sender<RunnerEvent>,
  /// `GITHUB_WORKSPACE` — base for resolving nested `./local` refs.
  pub workspace: &'a Path,
  /// Runner config (data dir, caches).
  pub config: &'a RunnerConfig,
  /// Shared depth tracker bounding composite recursion.
  pub depth: &'a mut DepthTracker,
  /// Job-level cancellation token; nested actions must stop on SIGINT/SIGTERM.
  pub cancel: &'a CancellationToken,
  /// Fixed enclosing step deadline.
  pub deadline: Option<Instant>,
  /// The job-scope HTTP client, reused for the nested action's resolution.
  pub http: &'a reqwest::Client,
  /// The job-scope single-flight action fetcher — the nested action's
  /// download joins any in-flight prefetch/step-time download for the same
  /// ref instead of starting a second one.
  pub fetcher: &'a ActionFetcher,
  /// The enclosing composite's own step id — the real top-level GH step id
  /// its `##[group]`/log lines already land under. The nested step's action
  /// header and stdout/stderr are routed here too: the listener only
  /// registers a per-step log uploader for a real top-level step id, and the
  /// synthetic id this nested step carries (`step.id` or
  /// `__composite_uses_{idx}`) has none.
  pub parent_step_id: &'a str,
}

/// Resolve and run a composite `uses:` step recursively.
///
/// The composite loop evaluates `if` before calling this function. Render
/// `with:` and step-local `env`, dispatch the nested action, and register its
/// post stage under the action instance's saved scope.
///
/// # Errors
///
/// Returns `RunnerError` if the nested action fails to resolve or execute.
pub async fn run_nested_uses_step(
  params: NestedUsesParams<'_>,
) -> Result<(Conclusion, HashMap<String, String>), RunnerError> {
  let Some(uses) = params.step.uses.as_deref() else {
    return Ok((Conclusion::Success, HashMap::new()));
  };

  let mut synthetic = build_nested_step(params.step, params.idx, uses, params.inputs, params.ctx)?;
  let step_env = render_nested_env(params.step, params.inputs, params.ctx)?;
  attach_rendered_env(&mut synthetic, step_env);

  // Recursive call: a nested composite re-enters `execute_action`, which
  // enters the depth tracker again, so the chain is bounded by `MAX_COMPOSITE_DEPTH`.
  // The enclosing step's deadline bounds nested subprocesses; the job cancel
  // token is shared so a top-level cancel kills the nested action too.
  let bounds = StepBounds::nested(
    params.deadline,
    synthetic.timeout_in_minutes,
    params.cancel.clone(),
  );
  let run = super::action_exec::ActionRun {
    events: params.events,
    workspace: params.workspace,
    config: params.config,
    bounds: &bounds,
    http: params.http,
    fetcher: params.fetcher,
    log_step_id: params.parent_step_id,
  };
  let outcome = Box::pin(super::action_exec::execute_action(
    &synthetic,
    params.ctx,
    &run,
    params.depth,
  ))
  .await;
  let outcome = outcome?;

  record_nested_result(params.ctx, &synthetic, &outcome);
  if let Some(post) = outcome.post {
    params.ctx.register_nested_post(post);
  }
  Ok((outcome.conclusion, outcome.outputs))
}

fn attach_rendered_env(step: &mut ActionStep, env: HashMap<String, String>) {
  step.environment = Some(TemplateToken {
    token_type: 2,
    d: Some(
      env
        .into_iter()
        .map(|(key, value)| DictEntry {
          key: literal_token(&key),
          value: literal_token(&value),
        })
        .collect(),
    ),
    ..TemplateToken::default()
  });
}

fn render_nested_env(
  step: &CompositeStep,
  inputs: &HashMap<String, String>,
  ctx: &ExecutionContext,
) -> Result<HashMap<String, String>, RunnerError> {
  let eval_ctx = composite_eval_context(ctx, inputs, None);
  step
    .env
    .iter()
    .map(|(key, value)| {
      interpolate_composite_expr(value, &eval_ctx, CompositeField::Env)
        .map(|rendered| (key.clone(), rendered))
    })
    .collect()
}

fn record_nested_result(
  ctx: &mut ExecutionContext,
  step: &ActionStep,
  outcome: &super::action_exec::ActionOutcome,
) {
  if let Some(name) = step.expression_name() {
    for (key, value) in &outcome.outputs {
      ctx.set_step_output(name, key, value);
    }
    ctx.set_step_outcome(name, outcome.conclusion);
    ctx.set_step_conclusion(name, outcome.conclusion);
  }
}

/// Build a synthetic [`ActionStep`] for a nested composite `uses:` step.
///
/// Parses `uses` via the shared `parse_action_ref` so local/remote/empty-ref
/// handling matches the top-level path, then reconstructs `name`/`git_ref` so
/// the resolver re-parses to the same ref. `with:` values are interpolated and
/// packed into the `inputs` mapping token (read back as `INPUT_*`).
///
/// # Errors
///
/// Returns `RunnerError` when the nested `uses:` is malformed.
fn build_nested_step(
  step: &CompositeStep,
  idx: usize,
  uses: &str,
  inputs: &HashMap<String, String>,
  ctx: &ExecutionContext,
) -> Result<ActionStep, RunnerError> {
  let id = step
    .id
    .clone()
    .unwrap_or_else(|| format!("__composite_uses_{idx}"));

  let (name, git_ref) = if uses.starts_with("docker://") {
    (uses.to_owned(), None)
  } else {
    let action_ref = super::actions::resolver::parse_action_ref(uses)?;
    nested_ref_parts(&action_ref)
  };

  let inputs_token = build_inputs_token(&step.with, inputs, ctx)?;

  Ok(ActionStep {
    id,
    step_type: Some("action".to_owned()),
    display_name_token: None,
    context_name: step.id.clone(),
    condition: step.condition.clone(),
    continue_on_error: Some(step.continue_on_error),
    timeout_in_minutes: None,
    reference: ActionStepDefinitionReference {
      ref_type: Some("repository".to_owned()),
      image: None,
      name: Some(name),
      git_ref,
      repository_type: None,
      path: None,
    },
    inputs: inputs_token,
    environment: None,
  })
}

/// Reconstruct the `(name, git_ref)` for the synthetic step's reference so the
/// resolver re-parses to the same ref: a local ref keeps its `./path` name with
/// no `git_ref`; a remote ref is `owner/repo[/subpath]` with its `git_ref`.
fn nested_ref_parts(action_ref: &super::actions::resolver::ActionRef) -> (String, Option<String>) {
  use super::actions::resolver::ActionRefKind;
  match action_ref.kind {
    ActionRefKind::Local => (action_ref.local_path.clone().unwrap_or_default(), None),
    ActionRefKind::Remote => {
      let mut name = format!("{}/{}", action_ref.owner, action_ref.repo);
      if let Some(subpath) = &action_ref.subpath {
        name.push('/');
        name.push_str(subpath);
      }
      (name, Some(action_ref.git_ref.clone()))
    },
  }
}

/// Build the `inputs` mapping token (read back as `INPUT_*`) from a composite
/// step's `with:` map, interpolating `${{ inputs.* }}` / `${{ steps.* }}`.
fn build_inputs_token(
  with: &HashMap<String, String>,
  inputs: &HashMap<String, String>,
  ctx: &ExecutionContext,
) -> Result<TemplateToken, RunnerError> {
  let eval_ctx = composite_eval_context(ctx, inputs, None);
  let entries: Vec<DictEntry<TemplateToken>> = with
    .iter()
    .map(|(k, v)| {
      let value = interpolate_composite_expr(v, &eval_ctx, CompositeField::Step)?;
      Ok(DictEntry {
        key: literal_token(k),
        value: literal_token(&value),
      })
    })
    .collect::<Result<_, RunnerError>>()?;

  Ok(TemplateToken {
    token_type: 2,
    d: Some(entries),
    ..TemplateToken::default()
  })
}

/// A type-0 literal string template token.
fn literal_token(s: &str) -> TemplateToken {
  TemplateToken {
    token_type: 0,
    lit: Some(s.to_owned()),
    ..TemplateToken::default()
  }
}
