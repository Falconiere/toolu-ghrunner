//! Step environment resolution and file-command result application.

use std::collections::HashMap;

use shared::{ActionStep, AnnotationLevel, LogStream, RunnerError, RunnerEvent, TemplateToken};
use tokio::sync::mpsc;

use super::context::ExecutionContext;
use super::file_commands::{BLOCKED_ENV_MESSAGE, FileCommandFailure, FileCommandManager};
use expressions::evaluator::EvalContext;
use expressions::types::ExprValue;

/// Extract step-level environment from the `environment` token, evaluating
/// every entry against the caller's single per-step `eval_ctx` snapshot
/// (S4) instead of a fresh one per entry.
pub(super) fn resolve_step_env(
  step: &ActionStep,
  ctx: &ExecutionContext,
  eval_ctx: &EvalContext,
) -> Result<HashMap<String, String>, RunnerError> {
  let Some(env_token) = &step.environment else {
    return Ok(HashMap::new());
  };
  let mut result = HashMap::new();
  let entries = env_token.d.as_deref().unwrap_or_default();
  for entry in entries {
    let Some(key) = entry.key.to_string_value() else {
      continue;
    };
    let value = env_token_to_string(&entry.value, ctx, eval_ctx)?;
    result.insert(key.to_owned(), value);
  }
  Ok(result)
}

/// Render a scalar template token to its final string, evaluating expressions once.
///
/// GitHub serializes `KEY: ${{ expr }}` as an expression token (type 3),
/// not a literal — reading only `to_string_value()` silently turned every
/// such value into `""` (live bug: `WHO=${{ inputs.who }}` came out
/// empty). Literals still pass through `interpolate_with` so an inline
/// `${{ }}` inside a literal keeps working; expressions are evaluated
/// with GitHub's string coercion. Bare scalars follow the same coercion
/// rules: booleans (type 5) render lowercase, numbers (type 6) drop a
/// trailing `.0`, and null (type 7) is the empty string.
pub(super) fn env_token_to_string(
  token: &TemplateToken,
  ctx: &ExecutionContext,
  eval_ctx: &EvalContext,
) -> Result<String, RunnerError> {
  match token.token_type {
    0 => ctx.interpolate_with(eval_ctx, token.lit.as_deref().unwrap_or_default()),
    3 => {
      let expr = token.expr.as_deref().unwrap_or_default();
      Ok(ctx.evaluate_with(eval_ctx, expr)?.coerce_to_string())
    },
    5 => Ok(ExprValue::Bool(token.bool_val.unwrap_or_default()).coerce_to_string()),
    6 => Ok(ExprValue::Number(token.num_val.unwrap_or_default()).coerce_to_string()),
    7 => Ok(String::new()),
    other => {
      tracing::warn!(
        token_type = other,
        "unsupported step env value token type — using empty string"
      );
      Ok(String::new())
    },
  }
}

/// Where one finished stage's file commands apply and report.
pub(super) struct FileCommandScope<'a> {
  /// Runtime step id that keys saved state.
  pub(super) step_id: &'a str,
  /// Visible `steps.<name>` key for outputs, if the stage exposes them.
  pub(super) expression_name: Option<&'a str>,
  /// Set when the stage's `GITHUB_STATE` is kept for its post stage.
  pub(super) state_id: Option<&'a str>,
  /// Timeline record that receives file-command errors.
  pub(super) report_id: &'a str,
  /// The job's event stream.
  pub(super) events: &'a mpsc::Sender<RunnerEvent>,
}

/// Process file commands after step execution; returns `GITHUB_OUTPUT`
/// values and whether a command failed the step. Does NOT record outputs on
/// `ctx` — the caller ([`apply_file_commands_and_merge_outputs`]) is the
/// single writer, so a `$GITHUB_OUTPUT` value can't be recorded once here and
/// again by a caller's own loop. Pairs read before a malformed entry are
/// applied, as upstream's enumerator does before it throws.
async fn apply_file_commands(
  scope: &FileCommandScope<'_>,
  file_cmds: &FileCommandManager,
  ctx: &mut ExecutionContext,
) -> (HashMap<String, String>, bool) {
  let results = match file_cmds.process().await {
    Ok(results) => results,
    Err(error) => {
      let message = format!("Unable to process file commands: {error}");
      emit_error_issue(scope.report_id, &message, ctx, scope.events).await;
      return (HashMap::new(), true);
    },
  };
  for (key, value) in results.env_vars {
    ctx.set_env(&key, &value);
  }
  for dir in results.path_additions {
    ctx.prepend_path(&dir);
  }
  if scope.state_id.is_some() {
    for (key, value) in results.state {
      ctx.set_step_state(scope.step_id, &key, &value);
    }
  }
  let failed = report_file_command_issues(
    &results.failures,
    results.blocked_env,
    scope.report_id,
    ctx,
    scope.events,
  )
  .await;
  (results.outputs, failed)
}

/// Apply file commands and merge output values for a visible expression name.
/// Saved state uses the runtime step ID within the current action scope.
/// Stdout outputs take precedence over file outputs with the same key.
/// Returns the merged outputs and whether a file command failed the step.
pub(super) async fn apply_file_commands_and_merge_outputs(
  scope: &FileCommandScope<'_>,
  stdout_outputs: HashMap<String, String>,
  file_cmds: &FileCommandManager,
  ctx: &mut ExecutionContext,
) -> (HashMap<String, String>, bool) {
  let (mut outputs, failed) = apply_file_commands(scope, file_cmds, ctx).await;
  outputs.extend(stdout_outputs);
  if let Some(name) = scope.expression_name {
    for (key, value) in &outputs {
      ctx.set_step_output(name, key, value);
    }
  }
  (outputs, failed)
}

/// Report file-command problems as upstream issues on `report_id`: one
/// blocked `NODE_OPTIONS` error per blocked name (these never fail the step),
/// then each failed command's two errors. Returns whether a command failed the
/// step.
pub(super) async fn report_file_command_issues(
  failures: &[FileCommandFailure],
  blocked_env: usize,
  report_id: &str,
  ctx: &ExecutionContext,
  events: &mpsc::Sender<RunnerEvent>,
) -> bool {
  for _ in 0..blocked_env {
    emit_error_issue(report_id, BLOCKED_ENV_MESSAGE, ctx, events).await;
  }
  for failure in failures {
    emit_error_issue(report_id, &failure.summary(), ctx, events).await;
    emit_error_issue(report_id, &failure.message, ctx, events).await;
  }
  !failures.is_empty()
}

/// Emit one masked `##[error]` log line plus its error annotation, the shape
/// upstream's `ExecutionContext.Error` produces.
async fn emit_error_issue(
  report_id: &str,
  message: &str,
  ctx: &ExecutionContext,
  events: &mpsc::Sender<RunnerEvent>,
) {
  let message = match ctx.masker().lock() {
    Ok(masker) => masker.mask(message).into_owned(),
    Err(poisoned) => poisoned.into_inner().mask(message).into_owned(),
  };
  let log = RunnerEvent::Log {
    step_id: report_id.to_owned(),
    line: format!("##[error]{message}"),
    stream: LogStream::Stdout,
  };
  let annotation = RunnerEvent::Annotation {
    step_id: report_id.to_owned(),
    level: AnnotationLevel::Error,
    message,
    file: None,
    line: None,
    col: None,
    end_line: None,
    end_column: None,
    title: None,
  };
  for event in [log, annotation] {
    if events.send(event).await.is_err() {
      tracing::warn!("event receiver closed while reporting a file-command error");
      return;
    }
  }
}
