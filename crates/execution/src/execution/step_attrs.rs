//! Execution-time evaluation of a step's `timeout-minutes` and
//! `continue-on-error` template tokens.
//!
//! Mirrors upstream `PipelineTemplateEvaluator.EvaluateStepTimeout` /
//! `EvaluateStepContinueOnError`: an expression token is evaluated against
//! the live context and its result must already be a number / boolean — no
//! coercion. Any failure becomes a [`StepAttrError`] that the caller logs
//! after the matching preface, falling back to no timeout / `false`.

use std::time::Duration;

use expressions::types::ExprValue;
use shared::{RunnerError, TemplateToken};

use super::context::ExecutionContext;

/// Logged before a `timeout-minutes` [`StepAttrError`].
pub const TIMEOUT_ERROR_PREFACE: &str =
  "An error occurred when attempting to determine the step timeout.";

/// Logged before a `continue-on-error` [`StepAttrError`].
pub const CONTINUE_ON_ERROR_PREFACE: &str = "The step failed and an error occurred when attempting to determine whether to continue on error.";

/// Largest whole-minute timeout the runner arms — the .NET `CancelAfter`
/// ceiling (`u32::MAX - 1` milliseconds) upstream is bound by.
pub const MAX_TIMEOUT_MINUTES: f64 = 71_582.0;

/// Why a step attribute could not be evaluated.
#[derive(Debug, Clone, PartialEq)]
pub enum StepAttrError {
  /// Upstream's template validation failure, positioned at the token.
  Template {
    /// 1-based index into the job message's `fileTable`.
    file: Option<i32>,
    /// Source line of the token.
    line: Option<i32>,
    /// Source column of the token.
    col: Option<i32>,
    /// The validation or expression-evaluation message.
    message: String,
  },
  /// `timeout-minutes` truncated to a whole number the runner cannot arm.
  TimeoutOutOfRange(f64),
}

impl StepAttrError {
  fn at(token: &TemplateToken, message: String) -> Self {
    Self::Template {
      file: token.file,
      line: token.line,
      col: token.col,
      message,
    }
  }

  /// The error line to log, resolving the token's file id through the job
  /// message's `fileTable` the way upstream's `TemplateContext` does.
  pub fn render(&self, file_table: &[serde_json::Value]) -> String {
    match self {
      Self::Template {
        file,
        line,
        col,
        message,
      } => {
        let name = file
          .and_then(|id| usize::try_from(id).ok())
          .and_then(|id| id.checked_sub(1))
          .and_then(|index| file_table.get(index))
          .and_then(serde_json::Value::as_str);
        let prefix = match (name, line, col) {
          (Some(name), Some(line), Some(col)) => format!("{name} (Line: {line}, Col: {col}): "),
          (Some(name), _, _) => format!("{name}: "),
          (None, Some(line), Some(col)) => format!("(Line: {line}, Col: {col}): "),
          _ => String::new(),
        };
        format!("The template is not valid. {prefix}{message}")
      },
      Self::TimeoutOutOfRange(minutes) => format!(
        "The step timeout of {} minutes is out of range; the maximum is {} minutes.",
        ExprValue::Number(*minutes).coerce_to_string(),
        ExprValue::Number(MAX_TIMEOUT_MINUTES).coerce_to_string(),
      ),
    }
  }
}

/// Resolve `timeout-minutes`. `Ok(None)` when absent, `null`, or not
/// positive after truncation toward zero (upstream's `(Int32)` cast).
///
/// # Errors
/// [`StepAttrError::Template`] when the value is not a number or the
/// expression fails; [`StepAttrError::TimeoutOutOfRange`] past
/// [`MAX_TIMEOUT_MINUTES`] or non-finite.
pub fn evaluate_timeout(
  token: Option<&TemplateToken>,
  ctx: &ExecutionContext,
) -> Result<Option<Duration>, StepAttrError> {
  let Some(token) = token.filter(|t| t.token_type != 7) else {
    return Ok(None);
  };
  let value = resolve(token, ctx)?;
  let ExprValue::Number(minutes) = value else {
    return Err(unexpected(token, &value));
  };
  let minutes = minutes.trunc();
  if !minutes.is_finite() || minutes > MAX_TIMEOUT_MINUTES {
    return Err(StepAttrError::TimeoutOutOfRange(minutes));
  }
  Ok((minutes > 0.0).then(|| Duration::from_secs_f64(minutes * 60.0)))
}

/// Resolve `continue-on-error`. `Ok(false)` when absent or `null`.
///
/// # Errors
/// [`StepAttrError::Template`] when the value is not a boolean or the
/// expression fails.
pub fn evaluate_continue_on_error(
  token: Option<&TemplateToken>,
  ctx: &ExecutionContext,
) -> Result<bool, StepAttrError> {
  let Some(token) = token.filter(|t| t.token_type != 7) else {
    return Ok(false);
  };
  let value = resolve(token, ctx)?;
  if let ExprValue::Bool(b) = value {
    Ok(b)
  } else {
    Err(unexpected(token, &value))
  }
}

fn resolve(token: &TemplateToken, ctx: &ExecutionContext) -> Result<ExprValue, StepAttrError> {
  match token.token_type {
    0 => Ok(ExprValue::String(token.lit.clone().unwrap_or_default())),
    1 => Err(StepAttrError::at(
      token,
      "A sequence was not expected".to_owned(),
    )),
    2 => Err(StepAttrError::at(
      token,
      "A mapping was not expected".to_owned(),
    )),
    3 => ctx
      .evaluate_expression(token.expr.as_deref().unwrap_or_default())
      .map_err(|e| {
        let message = if let RunnerError::Expression(message) = &e {
          message.clone()
        } else {
          e.to_string()
        };
        StepAttrError::at(token, message)
      }),
    5 => Ok(ExprValue::Bool(token.bool_val.unwrap_or_default())),
    6 => Ok(ExprValue::Number(token.num_val.unwrap_or_default())),
    _ => Ok(ExprValue::Null),
  }
}

fn unexpected(token: &TemplateToken, value: &ExprValue) -> StepAttrError {
  let message = match value {
    ExprValue::Array(_) => "A sequence was not expected".to_owned(),
    ExprValue::Object(_) => "A mapping was not expected".to_owned(),
    ExprValue::Null | ExprValue::Bool(_) | ExprValue::Number(_) | ExprValue::String(_) => {
      format!("Unexpected value '{}'", value.coerce_to_string())
    },
  };
  StepAttrError::at(token, message)
}

#[cfg(test)]
#[path = "tests/step_attrs.rs"]
mod tests;
