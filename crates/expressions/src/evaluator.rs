//! Tree evaluation with reference-preserving values and lazy branches.

use std::collections::HashMap;

use shared::RunnerError;

use super::parser::{BinaryOperator, Expr, UnaryOperator, parse};
use super::types::ExprValue;

/// Current job status for status functions (`success()`, `failure()`, etc.).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobStatus {
  /// All previous steps have succeeded (or none have failed).
  Success,
  /// A previous step has failed.
  Failure,
  /// The job has been cancelled.
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
  crate::validation::validate(&expr, ctx)?;
  eval_expr(&expr, ctx)
}

fn eval_expr(expr: &Expr, ctx: &EvalContext) -> Result<ExprValue, RunnerError> {
  match expr {
    Expr::Literal(value) => Ok(value.clone()),
    Expr::Context { name } => Ok(resolve_context(ctx, name)),
    Expr::PropertyAccess { object, property } => {
      let obj = eval_expr(object, ctx)?;
      Ok(crate::access::access(
        &obj,
        Some(&ExprValue::String(property.clone())),
      ))
    },
    Expr::IndexAccess { object, index } => {
      let obj = eval_expr(object, ctx)?;
      if obj.is_primitive() {
        return Ok(ExprValue::Null);
      }
      let idx = eval_expr(index, ctx)?;
      Ok(crate::access::access(&obj, Some(&idx)))
    },
    Expr::WildcardAccess { object } => {
      let obj = eval_expr(object, ctx)?;
      Ok(crate::access::access(&obj, None))
    },
    Expr::FunctionCall { name, args } => {
      if name.eq_ignore_ascii_case("case") {
        return eval_case(args, ctx);
      }
      if name.eq_ignore_ascii_case("format") {
        return eval_format(args, ctx);
      }
      let mut evaluated = Vec::with_capacity(args.len());
      for (index, arg) in args.iter().enumerate() {
        let unused = index == 1
          && evaluated
            .first()
            .is_some_and(|first| unused_second(name, first));
        evaluated.push(if unused {
          ExprValue::Null
        } else {
          eval_expr(arg, ctx)?
        });
      }
      super::functions::call_function(name, &evaluated, ctx)
    },
    Expr::UnaryOp { op, operand } => eval_unary(*op, operand, ctx),
    Expr::BinaryOp { op, left, right } => eval_binary(*op, left, right, ctx),
  }
}

fn unused_second(name: &str, first: &ExprValue) -> bool {
  match name.to_ascii_lowercase().as_str() {
    "contains" => {
      !first.is_primitive() && !matches!(first, ExprValue::Array(items) if !items.is_empty())
    },
    "startswith" | "endswith" => !first.is_primitive(),
    "join" => !matches!(first, ExprValue::Array(items) if items.len() > 1),
    _ => false,
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

fn eval_case(args: &[Expr], ctx: &EvalContext) -> Result<ExprValue, RunnerError> {
  if args.len().is_multiple_of(2) {
    return Err(RunnerError::Expression(
      "case requires an odd number of arguments".to_owned(),
    ));
  }
  for pair in args.chunks_exact(2) {
    let predicate = pair
      .first()
      .ok_or_else(|| RunnerError::Expression("case missing predicate".to_owned()))?;
    match eval_expr(predicate, ctx)? {
      ExprValue::Bool(false) => {},
      ExprValue::Bool(true) => {
        return eval_expr(
          pair
            .get(1)
            .ok_or_else(|| RunnerError::Expression("case missing result".to_owned()))?,
          ctx,
        );
      },
      ExprValue::Null
      | ExprValue::Number(_)
      | ExprValue::String(_)
      | ExprValue::Array(_)
      | ExprValue::Object(_) => {
        return Err(RunnerError::Expression(
          "case predicate must evaluate to a boolean".to_owned(),
        ));
      },
    }
  }
  eval_expr(
    args
      .last()
      .ok_or_else(|| RunnerError::Expression("case missing default".to_owned()))?,
    ctx,
  )
}

fn eval_format(args: &[Expr], ctx: &EvalContext) -> Result<ExprValue, RunnerError> {
  let template = eval_expr(
    args
      .first()
      .ok_or_else(|| RunnerError::Expression("format missing template".to_owned()))?,
    ctx,
  )?
  .coerce_to_string();
  crate::functions::format::render(&template, args.len().saturating_sub(1), |index| {
    eval_expr(
      args
        .get(index + 1)
        .ok_or_else(|| RunnerError::Expression("format missing argument".to_owned()))?,
      ctx,
    )
  })
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
    BinaryOperator::Lt => ordered(left, right, std::cmp::Ordering::Less),
    BinaryOperator::Le => left.loose_eq(right) || ordered(left, right, std::cmp::Ordering::Less),
    BinaryOperator::Gt => ordered(left, right, std::cmp::Ordering::Greater),
    BinaryOperator::Ge => left.loose_eq(right) || ordered(left, right, std::cmp::Ordering::Greater),
    BinaryOperator::And | BinaryOperator::Or => {
      return Err(RunnerError::Expression(
        "unexpected && or || in comparison".to_owned(),
      ));
    },
  };
  Ok(ExprValue::Bool(result))
}

fn ordered(left: &ExprValue, right: &ExprValue, order: std::cmp::Ordering) -> bool {
  match (left, right) {
    (ExprValue::String(a), ExprValue::String(b)) => crate::types::ordinal_compare(a, b) == order,
    (ExprValue::Null, ExprValue::Null) => false,
    _ => {
      left
        .coerce_to_number()
        .partial_cmp(&right.coerce_to_number())
        == Some(order)
    },
  }
}
