use shared::RunnerError;

use super::evaluator::EvalContext;

/// Process a string containing `${{ expression }}` placeholders.
///
/// Finds all `${{ ... }}` occurrences, evaluates each expression,
/// and replaces with the string representation of the result.
///
/// # Errors
///
/// Returns `RunnerError::Expression` on unclosed expressions or evaluation failures.
pub fn interpolate(input: &str, ctx: &EvalContext) -> Result<String, RunnerError> {
  let mut result = String::with_capacity(input.len());
  let mut rest = input;

  while let Some(start) = rest.find("${{") {
    result.push_str(rest.get(..start).unwrap_or_default());
    let after_open = rest.get(start + 3..).unwrap_or_default();
    let end = after_open
      .find("}}")
      .ok_or_else(|| RunnerError::Expression("unclosed ${{ expression".to_owned()))?;
    let expr_str = after_open.get(..end).unwrap_or_default().trim();
    let value = super::evaluator::evaluate(expr_str, ctx)?;
    result.push_str(&value.coerce_to_string());
    rest = after_open.get(end + 2..).unwrap_or_default();
  }
  result.push_str(rest);
  Ok(result)
}
