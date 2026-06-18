use std::collections::HashMap;

use shared::RunnerError;

use crate::execution::actions::manifest::ActionDefinition;
use crate::execution::composite_scope::{CompositeOutputs, ScopeName};
use crate::execution::depth_tracker::DepthTracker;

/// Execute a composite action's steps as a child execution scope.
///
/// Creates a scoped context for the composite's internal steps,
/// evaluates output expressions, and merges results to the parent.
///
/// # Errors
///
/// Returns `RunnerError::StepExecution` on depth limit or execution failures.
pub fn prepare_composite(
  action_def: &ActionDefinition,
  step_id: &str,
  depth: &mut DepthTracker,
) -> Result<CompositeExecution, RunnerError> {
  depth.enter()?;

  let scope = ScopeName::new(step_id);
  let outputs = CompositeOutputs::from_manifest(&action_def.outputs);

  Ok(CompositeExecution {
    scope,
    outputs,
    step_id: step_id.to_owned(),
  })
}

/// State for a composite action execution.
pub struct CompositeExecution {
  pub scope: ScopeName,
  pub outputs: CompositeOutputs,
  pub step_id: String,
}

impl CompositeExecution {
  /// Evaluate output expressions and return the final outputs map.
  ///
  /// In the full implementation, this evaluates `${{ steps.X.outputs.Y }}`
  /// expressions from the composite's `outputs:` section against the
  /// child execution context.
  pub fn evaluate_outputs(
    &self,
    child_step_outputs: &HashMap<String, HashMap<String, String>>,
  ) -> HashMap<String, String> {
    let mut result: HashMap<String, String> = HashMap::new();

    for (name, expr) in self.outputs.expressions() {
      // Simple evaluation: if the expression references a step output directly,
      // extract it. Full expression evaluation would use the expression engine.
      if let Some(value) = resolve_simple_output_ref(expr, child_step_outputs) {
        result.insert(name.clone(), value);
      }
    }

    result
  }
}

/// Resolve a simple output reference like `${{ steps.build.outputs.result }}`.
fn resolve_simple_output_ref(
  expr: &str,
  step_outputs: &HashMap<String, HashMap<String, String>>,
) -> Option<String> {
  let trimmed = expr.trim();
  let inner = trimmed
    .strip_prefix("${{")
    .and_then(|s| s.strip_suffix("}}"))
    .map(str::trim)
    .unwrap_or(trimmed);

  // Parse "steps.<id>.outputs.<key>"
  let parts: Vec<&str> = inner.split('.').collect();
  if parts.len() == 4
    && parts.first().copied() == Some("steps")
    && parts.get(2).copied() == Some("outputs")
  {
    let step_id = parts.get(1).copied().unwrap_or_default();
    let key = parts.get(3).copied().unwrap_or_default();
    return step_outputs
      .get(step_id)
      .and_then(|outputs| outputs.get(key))
      .cloned();
  }

  None
}
