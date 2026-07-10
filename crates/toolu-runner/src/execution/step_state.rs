//! Per-step output/conclusion state and its `steps` expression-context snapshot.

use std::collections::HashMap;

use shared::Conclusion;

use super::expressions::types::ExprValue;

/// Recorded outputs, saved state, and final outcome for a single executed step.
#[derive(Default)]
pub(super) struct StepState {
  pub(super) outputs: HashMap<String, String>,
  /// `save-state` / `STATE_*` values, surfaced to the action's post step.
  pub(super) state: HashMap<String, String>,
  /// The step's REAL result, before `continue-on-error` adjustment.
  pub(super) outcome: Option<Conclusion>,
  /// The effective result after `continue-on-error`: equals `outcome` unless
  /// the step failed with `continue-on-error: true`, then `Success`.
  pub(super) conclusion: Option<Conclusion>,
}

/// Build the `steps` expression context from recorded per-step state.
pub(super) fn build_steps_context(steps: &HashMap<String, StepState>) -> ExprValue {
  let mut steps_map = HashMap::new();
  for (id, state) in steps {
    let mut step_obj = HashMap::new();

    // outputs
    let outputs: HashMap<String, ExprValue> = state
      .outputs
      .iter()
      .map(|(k, v)| (k.clone(), ExprValue::String(v.clone())))
      .collect();
    step_obj.insert("outputs".to_owned(), ExprValue::Object(outputs));

    // state (`save-state` values; surfaced as `STATE_*` to the post stage and
    // exposed here so `${{ steps.<id>.state.<k> }}` resolves).
    let state_map: HashMap<String, ExprValue> = state
      .state
      .iter()
      .map(|(k, v)| (k.clone(), ExprValue::String(v.clone())))
      .collect();
    step_obj.insert("state".to_owned(), ExprValue::Object(state_map));

    // outcome = real result; conclusion = continue-on-error-adjusted result.
    // They differ when a step failed under `continue-on-error: true`.
    let outcome_str = state.outcome.map(conclusion_to_string).unwrap_or_default();
    let conclusion_str = state
      .conclusion
      .map(conclusion_to_string)
      .unwrap_or_default();
    step_obj.insert(
      "outcome".to_owned(),
      ExprValue::String(outcome_str.to_owned()),
    );
    step_obj.insert(
      "conclusion".to_owned(),
      ExprValue::String(conclusion_str.to_owned()),
    );

    steps_map.insert(id.clone(), ExprValue::Object(step_obj));
  }
  ExprValue::Object(steps_map)
}

fn conclusion_to_string(c: Conclusion) -> &'static str {
  match c {
    Conclusion::Success => "success",
    Conclusion::Failure => "failure",
    Conclusion::Cancelled => "cancelled",
    Conclusion::Skipped => "skipped",
  }
}
