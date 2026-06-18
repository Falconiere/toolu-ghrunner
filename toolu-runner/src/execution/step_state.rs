//! Per-step output/conclusion state and its `steps` expression-context snapshot.

use std::collections::HashMap;

use shared::Conclusion;

use super::expressions::types::ExprValue;

/// Recorded outputs and final outcome for a single executed step.
pub(super) struct StepState {
  pub(super) outputs: HashMap<String, String>,
  pub(super) outcome: Option<Conclusion>,
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

    // outcome
    let outcome_str = state.outcome.map(conclusion_to_string).unwrap_or_default();
    step_obj.insert(
      "outcome".to_owned(),
      ExprValue::String(outcome_str.to_owned()),
    );
    step_obj.insert(
      "conclusion".to_owned(),
      ExprValue::String(outcome_str.to_owned()),
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
