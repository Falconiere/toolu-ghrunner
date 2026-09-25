//! Evaluate acquired workflow/job environment layers before container setup.

use std::collections::HashMap;

use expressions::evaluator::EvalContext;
use expressions::types::ExprValue;
use shared::{RunnerError, TemplateToken};

use super::context::ExecutionContext;

/// Evaluate each complete mapping against the preceding layer's snapshot.
pub(super) fn apply_job_environment(
  layers: &[TemplateToken],
  ctx: &mut ExecutionContext,
) -> Result<(), RunnerError> {
  for (index, layer) in layers.iter().enumerate() {
    let values = evaluate_layer(layer, ctx).map_err(|error| {
      // Expression errors may contain source literals, including secrets. Keep
      // the setup diagnostic useful without copying the untrusted expression.
      let reason = if matches!(error, RunnerError::Protocol(_)) {
        "expected a mapping with scalar keys and values"
      } else {
        "expression evaluation failed"
      };
      RunnerError::Expression(format!("environmentVariables layer {index}: {reason}"))
    })?;
    for (key, value) in values {
      ctx.set_env(&key, &value);
    }
  }
  Ok(())
}

fn evaluate_layer(
  token: &TemplateToken,
  ctx: &ExecutionContext,
) -> Result<HashMap<String, String>, RunnerError> {
  if token.token_type == 7 {
    return Ok(HashMap::new());
  }
  if token.token_type != 2 {
    return Err(invalid_token());
  }
  // The upstream serializer omits the payload of an empty mapping.
  let entries = token.d.as_deref().unwrap_or_default();
  let snapshot = ctx.eval_context();
  let mut values = HashMap::with_capacity(entries.len());
  for entry in entries {
    let key = scalar(&entry.key, ctx, &snapshot)?;
    let value = scalar(&entry.value, ctx, &snapshot)?;
    values.insert(key, value);
  }
  Ok(values)
}

fn scalar(
  token: &TemplateToken,
  ctx: &ExecutionContext,
  snapshot: &EvalContext,
) -> Result<String, RunnerError> {
  let value = match token.token_type {
    // A wire literal is already evaluated. Re-interpolating it can turn data
    // containing `${{ ... }}` into unintended expression execution.
    0 => ExprValue::String(token.lit.clone().unwrap_or_default()),
    3 => ctx.evaluate_with(snapshot, token.expr.as_deref().ok_or_else(invalid_token)?)?,
    // Upstream omits false/zero payloads. Null is a valid empty env string.
    5 => ExprValue::Bool(token.bool_val.unwrap_or_default()),
    6 => ExprValue::Number(token.num_val.unwrap_or_default()),
    7 => ExprValue::Null,
    // Numeric wire tags are open-ended; mappings, sequences, directives and
    // unknown tags cannot be scalar environment keys or values.
    _ => return Err(invalid_token()),
  };
  match value {
    ExprValue::Array(_) | ExprValue::Object(_) => Err(invalid_token()),
    ExprValue::Null | ExprValue::Bool(_) | ExprValue::Number(_) | ExprValue::String(_) => {
      Ok(value.coerce_to_string())
    },
  }
}

fn invalid_token() -> RunnerError {
  RunnerError::Protocol("invalid environment template token".to_owned())
}
