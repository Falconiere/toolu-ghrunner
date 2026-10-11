//! End-of-job `environment.url` evaluation, mirroring upstream
//! `JobExtension.FinalizeJob` + `PipelineTemplateEvaluator.EvaluateEnvironmentUrl`
//! (actions/runner cab9d1c).
//!
//! The URL template is evaluated against schema
//! `string-runner-context-no-secrets`: only the named contexts below exist, so
//! `secrets.X` is an "Unrecognized named-value" error. A scalar result becomes
//! the string upstream's `TemplateEvaluator.Validate` produces (`null` → `""`,
//! booleans/numbers → their literal text); a sequence or mapping fails. A value
//! the job's masker would change is suppressed — including a literal one,
//! a deliberate deviation so a registered secret never reaches `completejob`.

use expressions::types::ExprValue;
use shared::{ActionsEnvironment, RunnerError, TemplateToken};

use super::context::ExecutionContext;
use super::step_attrs::StepAttrError;

/// The contexts upstream's `string-runner-context-no-secrets` schema allows.
const ALLOWED_CONTEXTS: [&str; 9] = [
  "github", "needs", "strategy", "matrix", "steps", "job", "runner", "env", "vars",
];

/// What the job reports for its environment URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EnvironmentUrl {
  /// No environment, no `url`, or a null token: nothing is evaluated.
  Absent,
  /// The evaluated, secret-free URL to send as `environmentUrl`.
  Url(String),
  /// The value contains a registered secret and is not reported.
  Secret,
  /// Evaluation failed; the rendered upstream template error.
  Error(String),
}

/// Evaluate the job's `environment.url` against the final job context.
pub(crate) fn evaluate_environment_url(
  environment: Option<&ActionsEnvironment>,
  ctx: &ExecutionContext,
) -> EnvironmentUrl {
  let Some(token) = environment
    .and_then(|environment| environment.url.as_ref())
    .filter(|token| token.token_type != NULL_TOKEN)
  else {
    return EnvironmentUrl::Absent;
  };
  let value = match evaluate(token, ctx) {
    Ok(value) => value,
    Err(error) => return EnvironmentUrl::Error(error.render(ctx.file_table())),
  };
  let masked = match ctx.masker().lock() {
    Ok(guard) => guard.mask(&value).into_owned(),
    Err(poisoned) => poisoned.into_inner().mask(&value).into_owned(),
  };
  if masked == value {
    EnvironmentUrl::Url(value)
  } else {
    EnvironmentUrl::Secret
  }
}

// Wire discriminants from `shared::TemplateToken`.
const LITERAL_TOKEN: i32 = 0;
const SEQUENCE_TOKEN: i32 = 1;
const MAPPING_TOKEN: i32 = 2;
const EXPRESSION_TOKEN: i32 = 3;
const BOOLEAN_TOKEN: i32 = 5;
const NUMBER_TOKEN: i32 = 6;
const NULL_TOKEN: i32 = 7;

fn evaluate(token: &TemplateToken, ctx: &ExecutionContext) -> Result<String, StepAttrError> {
  let value = match token.token_type {
    LITERAL_TOKEN => ExprValue::String(token.lit.clone().unwrap_or_default()),
    SEQUENCE_TOKEN => return Err(not_expected(token, "sequence")),
    MAPPING_TOKEN => return Err(not_expected(token, "mapping")),
    EXPRESSION_TOKEN => {
      let mut eval_ctx = ctx.eval_context();
      eval_ctx
        .contexts
        .retain(|name, _| ALLOWED_CONTEXTS.contains(&name.as_str()));
      ctx
        .evaluate_with(&eval_ctx, token.expr.as_deref().unwrap_or_default())
        .map_err(|error| {
          let message = if let RunnerError::Expression(message) = &error {
            message.clone()
          } else {
            error.to_string()
          };
          StepAttrError::at(token, message)
        })?
    },
    BOOLEAN_TOKEN => ExprValue::Bool(token.bool_val.unwrap_or_default()),
    NUMBER_TOKEN => ExprValue::Number(token.num_val.unwrap_or_default()),
    _ => ExprValue::Null,
  };
  match value {
    ExprValue::String(text) => Ok(text),
    ExprValue::Null => Ok(String::new()),
    ExprValue::Bool(_) | ExprValue::Number(_) => Ok(value.coerce_to_string()),
    ExprValue::Array(_) => Err(not_expected(token, "sequence")),
    ExprValue::Object(_) => Err(not_expected(token, "mapping")),
  }
}

fn not_expected(token: &TemplateToken, shape: &str) -> StepAttrError {
  StepAttrError::at(token, format!("A {shape} was not expected"))
}
