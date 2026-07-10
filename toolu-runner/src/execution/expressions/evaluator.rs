use std::collections::HashMap;

use shared::RunnerError;

use super::parser::{BinaryOperator, Expr, UnaryOperator, parse};
use super::types::ExprValue;

/// Current job status for status functions (success(), failure(), etc.).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobStatus {
  Success,
  Failure,
  Cancelled,
}

/// Context available during expression evaluation.
pub struct EvalContext {
  /// Named context objects: github, env, secrets, steps, matrix, etc.
  pub contexts: HashMap<String, ExprValue>,
  /// Current job status for status functions.
  pub job_status: JobStatus,
  /// Job workspace root that `hashFiles()` resolves patterns against.
  /// `None` outside a job (workflow-level evaluation), where it is an error.
  pub workspace: Option<std::path::PathBuf>,
}

/// Evaluate a GitHub Actions expression string against a context.
///
/// # Errors
///
/// Returns `RunnerError::Expression` on parse or evaluation errors.
pub fn evaluate(input: &str, ctx: &EvalContext) -> Result<ExprValue, RunnerError> {
  let expr = parse(input)?;
  eval_expr(&expr, ctx)
}

fn eval_expr(expr: &Expr, ctx: &EvalContext) -> Result<ExprValue, RunnerError> {
  match expr {
    Expr::Literal(value) => Ok(value.clone()),
    Expr::Context { name } => Ok(resolve_context(ctx, name)),
    Expr::PropertyAccess { object, property } => {
      let obj = eval_expr(object, ctx)?;
      Ok(access_property(&obj, property))
    },
    Expr::IndexAccess { object, index } => {
      let obj = eval_expr(object, ctx)?;
      let idx = eval_expr(index, ctx)?;
      Ok(access_index(&obj, &idx))
    },
    Expr::WildcardAccess { object } => {
      let obj = eval_expr(object, ctx)?;
      Ok(wildcard_access(&obj))
    },
    Expr::FunctionCall { name, args } => {
      let evaluated: Vec<ExprValue> = args
        .iter()
        .map(|a| eval_expr(a, ctx))
        .collect::<Result<_, _>>()?;
      super::functions::call_function(name, &evaluated, ctx)
    },
    Expr::UnaryOp { op, operand } => eval_unary(*op, operand, ctx),
    Expr::BinaryOp { op, left, right } => eval_binary(*op, left, right, ctx),
  }
}

/// Case-insensitive context lookup.
fn resolve_context(ctx: &EvalContext, name: &str) -> ExprValue {
  let lower = name.to_ascii_lowercase();
  for (k, v) in &ctx.contexts {
    if k.to_ascii_lowercase() == lower {
      return v.clone();
    }
  }
  ExprValue::Null
}

/// Case-insensitive property access. Returns Null for missing/non-objects.
fn access_property(obj: &ExprValue, property: &str) -> ExprValue {
  match obj {
    ExprValue::Object(map) => {
      let lower = property.to_ascii_lowercase();
      for (k, v) in map {
        if k.to_ascii_lowercase() == lower {
          return v.clone();
        }
      }
      ExprValue::Null
    },
    ExprValue::Null
    | ExprValue::Bool(_)
    | ExprValue::Number(_)
    | ExprValue::String(_)
    | ExprValue::Array(_) => ExprValue::Null,
  }
}

fn access_index(obj: &ExprValue, idx: &ExprValue) -> ExprValue {
  match obj {
    ExprValue::Array(arr) => {
      let i = idx.coerce_to_number();
      if i.is_finite() && i >= 0.0 {
        let index = safe_f64_to_usize(i);
        arr.get(index).cloned().unwrap_or(ExprValue::Null)
      } else {
        ExprValue::Null
      }
    },
    ExprValue::Object(_) => {
      let key = idx.coerce_to_string();
      access_property(obj, &key)
    },
    ExprValue::Null | ExprValue::Bool(_) | ExprValue::Number(_) | ExprValue::String(_) => {
      ExprValue::Null
    },
  }
}

/// Wildcard access: collect all values from array/object.
fn wildcard_access(obj: &ExprValue) -> ExprValue {
  match obj {
    ExprValue::Array(arr) => ExprValue::Array(arr.clone()),
    ExprValue::Object(map) => ExprValue::Array(map.values().cloned().collect()),
    ExprValue::Null | ExprValue::Bool(_) | ExprValue::Number(_) | ExprValue::String(_) => {
      ExprValue::Null
    },
  }
}

fn eval_unary(
  op: UnaryOperator,
  operand: &Expr,
  ctx: &EvalContext,
) -> Result<ExprValue, RunnerError> {
  let val = eval_expr(operand, ctx)?;
  match op {
    UnaryOperator::Not => Ok(ExprValue::Bool(!val.is_truthy())),
  }
}

/// Binary operator evaluation with short-circuit for && and ||.
fn eval_binary(
  op: BinaryOperator,
  left: &Expr,
  right: &Expr,
  ctx: &EvalContext,
) -> Result<ExprValue, RunnerError> {
  let left_val = eval_expr(left, ctx)?;

  match op {
    BinaryOperator::And => {
      if !left_val.is_truthy() {
        return Ok(left_val);
      }
      eval_expr(right, ctx)
    },
    BinaryOperator::Or => {
      if left_val.is_truthy() {
        return Ok(left_val);
      }
      eval_expr(right, ctx)
    },
    BinaryOperator::Eq
    | BinaryOperator::Neq
    | BinaryOperator::Lt
    | BinaryOperator::Le
    | BinaryOperator::Gt
    | BinaryOperator::Ge => {
      let right_val = eval_expr(right, ctx)?;
      eval_comparison(op, &left_val, &right_val)
    },
  }
}

fn eval_comparison(
  op: BinaryOperator,
  left: &ExprValue,
  right: &ExprValue,
) -> Result<ExprValue, RunnerError> {
  let result = match op {
    BinaryOperator::Eq => left.loose_eq(right),
    BinaryOperator::Neq => !left.loose_eq(right),
    BinaryOperator::Lt => compare_numbers(left, right, |a, b| a < b),
    BinaryOperator::Le => compare_numbers(left, right, |a, b| a <= b),
    BinaryOperator::Gt => compare_numbers(left, right, |a, b| a > b),
    BinaryOperator::Ge => compare_numbers(left, right, |a, b| a >= b),
    BinaryOperator::And | BinaryOperator::Or => {
      return Err(RunnerError::Expression(
        "unexpected && or || in comparison".to_owned(),
      ));
    },
  };
  Ok(ExprValue::Bool(result))
}

fn compare_numbers(left: &ExprValue, right: &ExprValue, cmp: fn(f64, f64) -> bool) -> bool {
  let a = left.coerce_to_number();
  let b = right.coerce_to_number();
  if a.is_nan() || b.is_nan() {
    return false;
  }
  cmp(a, b)
}

/// Safely convert a non-negative finite f64 to usize.
fn safe_f64_to_usize(n: f64) -> usize {
  // Convert through string to avoid as-cast truncation/sign lints
  let s = format!("{}", n.floor());
  s.parse::<usize>().unwrap_or(0)
}
