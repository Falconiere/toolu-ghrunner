//! Validate names and function contracts before lazy evaluation.

use crate::evaluator::EvalContext;
use crate::parser::Expr;
use shared::RunnerError;

/// Check all names and call arities, including unselected branches.
pub(crate) fn validate(expr: &Expr, ctx: &EvalContext) -> Result<(), RunnerError> {
  match expr {
    Expr::Literal(_) => Ok(()),
    Expr::Context { name } => {
      if ctx
        .contexts
        .keys()
        .any(|key| key.eq_ignore_ascii_case(name))
      {
        Ok(())
      } else {
        Err(RunnerError::Expression(format!(
          "unknown named value: {name}"
        )))
      }
    },
    Expr::PropertyAccess { object, .. } | Expr::WildcardAccess { object } => validate(object, ctx),
    Expr::IndexAccess { object, index } => {
      validate(object, ctx)?;
      validate(index, ctx)
    },
    Expr::FunctionCall { name, args } => {
      validate_arity(name, args.len())?;
      for arg in args {
        validate(arg, ctx)?;
      }
      Ok(())
    },
    Expr::UnaryOp { operand, .. } => validate(operand, ctx),
    Expr::BinaryOp { left, right, .. } => {
      validate(left, ctx)?;
      validate(right, ctx)
    },
  }
}

/// Check builtin arity independently of argument evaluation.
pub(crate) fn validate_arity(name: &str, count: usize) -> Result<(), RunnerError> {
  let (min, max) = match name.to_ascii_lowercase().as_str() {
    "case" => (3, 255),
    "contains" | "startswith" | "endswith" => (2, 2),
    "format" | "hashfiles" => (1, 255),
    "join" => (1, 2),
    "tojson" | "fromjson" => (1, 1),
    "success" | "failure" | "cancelled" | "always" => (0, 0),
    _ => return Err(RunnerError::Expression(format!("unknown function: {name}"))),
  };
  if !(min..=max).contains(&count) {
    return Err(RunnerError::Expression(format!(
      "{name} expects {min}..={max} arguments, got {count}"
    )));
  }
  if name.eq_ignore_ascii_case("case") && count.is_multiple_of(2) {
    return Err(RunnerError::Expression(
      "case expects an odd number of arguments".to_owned(),
    ));
  }
  Ok(())
}
