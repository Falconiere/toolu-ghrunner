//! Full expression rendering and field policies for composite action metadata.

use std::collections::HashMap;

use expressions::evaluator::{EvalContext, evaluate};
use expressions::parser::{Expr, parse};
use expressions::template::interpolate;
use expressions::types::ExprValue;
use shared::RunnerError;

use super::context::ExecutionContext;

/// The pinned action metadata schema gives each field its own expression roots.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum CompositeField {
  /// A `run`, `with`, `shell`, working-directory, or name value.
  Step,
  /// An inner step's environment value (`github.action_path` is allowed).
  Env,
  /// An inner step's condition (status functions are allowed).
  If,
  /// A declared composite output value.
  Output,
  /// A manifest input default.
  InputDefault,
}

/// Snapshot the live job context and replace workflow inputs with this action's inputs.
pub(super) fn composite_eval_context(
  ctx: &ExecutionContext,
  inputs: &HashMap<String, String>,
  env: Option<&HashMap<String, String>>,
) -> EvalContext {
  let mut snapshot = ctx.eval_context();
  snapshot
    .contexts
    .insert("inputs".to_owned(), string_object(inputs));
  if let Some(env) = env {
    snapshot
      .contexts
      .insert("env".to_owned(), string_object(env));
  }
  snapshot
}

fn string_object(values: &HashMap<String, String>) -> ExprValue {
  ExprValue::Object(
    values
      .iter()
      .map(|(key, value)| (key.clone(), ExprValue::String(value.clone())))
      .collect(),
  )
}

/// Render a composite metadata string through the full expression engine.
pub(super) fn interpolate_composite_expr(
  text: &str,
  eval_ctx: &EvalContext,
  field: CompositeField,
) -> Result<String, RunnerError> {
  if !text.contains("${{") {
    return Ok(text.to_owned());
  }
  validate_template(text, field)?;
  interpolate(text, eval_ctx)
}

/// Evaluate an inner step `if` with an implicit `success()` when absent.
pub(super) fn evaluate_composite_condition(
  condition: Option<&str>,
  eval_ctx: &EvalContext,
) -> Result<bool, RunnerError> {
  let condition = condition.unwrap_or("success()").trim();
  let expression = if let Some(inner) = condition.strip_prefix("${{") {
    inner
      .strip_suffix("}}")
      .ok_or_else(|| RunnerError::Expression("unclosed composite if expression".to_owned()))?
      .trim()
  } else if condition.is_empty() {
    "success()"
  } else {
    condition
  };
  validate_expression(expression, CompositeField::If)?;
  Ok(evaluate(expression, eval_ctx)?.is_truthy())
}

fn validate_template(text: &str, field: CompositeField) -> Result<(), RunnerError> {
  let mut rest = text;
  while let Some(start) = rest.find("${{") {
    let after_open = rest.get(start + 3..).unwrap_or_default();
    let end = after_open
      .find("}}")
      .ok_or_else(|| RunnerError::Expression("unclosed ${{ expression".to_owned()))?;
    validate_expression(after_open.get(..end).unwrap_or_default().trim(), field)?;
    rest = after_open.get(end + 2..).unwrap_or_default();
  }
  Ok(())
}

fn validate_expression(expression: &str, field: CompositeField) -> Result<(), RunnerError> {
  let ast = parse(expression)?;
  validate_ast(&ast, field)
}

fn validate_ast(ast: &Expr, field: CompositeField) -> Result<(), RunnerError> {
  match ast {
    Expr::Literal(_) => Ok(()),
    Expr::Context { name } => validate_root(name, field),
    Expr::PropertyAccess { object, property } => {
      validate_action_path(object, property, field)?;
      validate_ast(object, field)
    },
    Expr::IndexAccess { object, index } => {
      if let Expr::Literal(ExprValue::String(property)) = index.as_ref() {
        validate_action_path(object, property, field)?;
      }
      validate_ast(object, field)?;
      validate_ast(index, field)
    },
    Expr::WildcardAccess { object } => validate_ast(object, field),
    Expr::FunctionCall { name, args } => {
      validate_function(name, field)?;
      for arg in args {
        validate_ast(arg, field)?;
      }
      Ok(())
    },
    Expr::UnaryOp { operand, .. } => validate_ast(operand, field),
    Expr::BinaryOp { left, right, .. } => {
      validate_ast(left, field)?;
      validate_ast(right, field)
    },
  }
}

fn validate_root(name: &str, field: CompositeField) -> Result<(), RunnerError> {
  let name = name.to_ascii_lowercase();
  let allowed = match field {
    CompositeField::InputDefault => {
      matches!(
        name.as_str(),
        "github" | "strategy" | "matrix" | "job" | "runner"
      )
    },
    CompositeField::Step | CompositeField::Env | CompositeField::If | CompositeField::Output => {
      matches!(
        name.as_str(),
        "github" | "inputs" | "strategy" | "matrix" | "steps" | "job" | "runner" | "env"
      )
    },
  };
  if allowed {
    Ok(())
  } else {
    Err(RunnerError::Expression(format!(
      "context '{name}' is unavailable in composite metadata"
    )))
  }
}

fn validate_function(name: &str, field: CompositeField) -> Result<(), RunnerError> {
  let lower = name.to_ascii_lowercase();
  let allowed = match lower.as_str() {
    "always" | "failure" | "cancelled" | "success" => field == CompositeField::If,
    "hashfiles" => field != CompositeField::Output,
    _ => true,
  };
  if allowed {
    Ok(())
  } else {
    Err(RunnerError::Expression(format!(
      "function '{name}' is unavailable in this composite field"
    )))
  }
}

fn validate_action_path(
  object: &Expr,
  property: &str,
  field: CompositeField,
) -> Result<(), RunnerError> {
  if field != CompositeField::Env
    && property.eq_ignore_ascii_case("action_path")
    && matches!(object, Expr::Context { name } if name.eq_ignore_ascii_case("github"))
  {
    return Err(RunnerError::Expression(
      "github.action_path is available only in composite step env".to_owned(),
    ));
  }
  Ok(())
}
