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
use tokio_util::sync::CancellationToken;

use super::actions::manifest::CompositeStep;
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
  /// Outputs of earlier composite steps (for `${{ steps.X.outputs.Y }}`).
  pub step_outputs: &'a HashMap<String, HashMap<String, String>>,
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
  /// Temp dir for `${{ runner.temp }}` expansion in `with:` values.
  pub temp_dir: &'a Path,
  /// Job-level cancellation token; nested actions must stop on SIGINT/SIGTERM.
  pub cancel: &'a CancellationToken,
}

/// Resolve and run a composite `uses:` step recursively.
///
/// Honors the step's `if` (via `should_skip`), `with:` (as the nested action's
/// inputs), per-step `env`, and `id`. Returns the nested action's conclusion;
/// `Success` when skipped by `if`.
///
/// # Errors
///
/// Returns `RunnerError` if the nested action fails to resolve or execute.
pub async fn run_nested_uses_step(
  params: NestedUsesParams<'_>,
  should_skip: bool,
) -> Result<Conclusion, RunnerError> {
  if should_skip {
    return Ok(Conclusion::Success);
  }

  let Some(uses) = params.step.uses.as_deref() else {
    return Ok(Conclusion::Success);
  };

  let synthetic = build_nested_step(
    params.step,
    params.idx,
    uses,
    params.inputs,
    params.step_outputs,
    params.temp_dir,
  )?;

  // Per-step `env` is applied to the live context for the nested action's run.
  for (k, v) in &params.step.env {
    params.ctx.set_env(k, v);
  }

  // Recursive call: a nested composite re-enters `execute_action`, which
  // enters the depth tracker again, so the chain is bounded by `MAX_COMPOSITE_DEPTH`.
  // The nested step's own `timeout-minutes` bounds its node children; the job
  // cancel token is shared so a top-level cancel kills the nested action too.
  let bounds = StepBounds::new(synthetic.timeout_in_minutes, params.cancel.clone());
  let run = super::action_exec::ActionRun {
    events: params.events,
    workspace: params.workspace,
    config: params.config,
    bounds: &bounds,
  };
  let outcome = Box::pin(super::action_exec::execute_action(
    &synthetic,
    params.ctx,
    &run,
    params.depth,
  ))
  .await?;

  Ok(outcome.conclusion)
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
  step_outputs: &HashMap<String, HashMap<String, String>>,
  temp_dir: &Path,
) -> Result<ActionStep, RunnerError> {
  let id = step
    .id
    .clone()
    .unwrap_or_else(|| format!("__composite_uses_{idx}"));

  let action_ref = super::actions::resolver::parse_action_ref(uses)?;
  let (name, git_ref) = nested_ref_parts(&action_ref);

  let inputs_token = build_inputs_token(&step.with, inputs, step_outputs, temp_dir);

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
  step_outputs: &HashMap<String, HashMap<String, String>>,
  temp_dir: &Path,
) -> TemplateToken {
  let entries: Vec<DictEntry<TemplateToken>> = with
    .iter()
    .map(|(k, v)| {
      let value = super::composite_expr::interpolate_composite_expr(
        v,
        inputs,
        step_outputs,
        &HashMap::new(),
        temp_dir,
      );
      DictEntry {
        key: literal_token(k),
        value: literal_token(&value),
      }
    })
    .collect();

  TemplateToken {
    token_type: 2,
    d: Some(entries),
    ..TemplateToken::default()
  }
}

/// A type-0 literal string template token.
fn literal_token(s: &str) -> TemplateToken {
  TemplateToken {
    token_type: 0,
    lit: Some(s.to_owned()),
    ..TemplateToken::default()
  }
}
