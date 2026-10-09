//! Step display names the way upstream's `ActionRunner.GenerateDisplayName`
//! produces them.
//!
//! Every step is named once at job start from the job message's contexts
//! alone, then, while still unevaluated, once more with its live contexts
//! right before its condition. An expression whose contexts or functions
//! are not all available yet shows its prettified text; one that fails to
//! evaluate logs upstream's warning and keeps the earlier name (`run` when
//! job start already failed). A fully evaluated name is masked.

use expressions::display::{display_string, first_line, references};
use expressions::evaluator::EvalContext;
use expressions::types::ExprValue;
use shared::{ActionStep, LogStream, RunnerError, RunnerEvent, SETUP_STEP_ID, TemplateToken};
use tokio::sync::mpsc;

use super::context::ExecutionContext;
use super::step_attrs::StepAttrError;

#[cfg(test)]
#[path = "tests/step_display.rs"]
mod tests;

/// Upstream's `DisplayName` when generation produced nothing.
pub const FALLBACK_NAME: &str = "run";

/// Prefix for names derived from the step's action or script.
const RUN_PREFIX: &str = "Run ";

/// Contexts the runner adds per step; the job message does not carry them.
const STEP_CONTEXTS: [&str; 4] = ["env", "steps", "runner", "secrets"];

/// Upstream's `ExpressionConstants.WellKnownFunctions`.
const WELL_KNOWN_FUNCTIONS: [&str; 8] = [
  "case",
  "contains",
  "endsWith",
  "format",
  "fromJSON",
  "join",
  "startsWith",
  "toJSON",
];

/// Functions upstream registers per step rather than at job start.
const STEP_FUNCTIONS: [&str; 5] = ["always", "cancelled", "failure", "success", "hashFiles"];

/// A top-level step's names across both evaluation attempts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepDisplay {
  /// The job-start name, which upstream's `Pre` stage keeps.
  pub initial: String,
  /// The name the step reports.
  pub current: String,
  evaluated: bool,
}

/// One generation attempt, already formatted with its `Run ` prefix.
enum Generated {
  Evaluated(String),
  Pending(String),
  Failed(String),
}

/// Name every step from the job message's contexts, logging failures to
/// "Set up job" like upstream's `JobExtension.InitializeJob`.
pub(crate) async fn name_at_job_start(
  steps: &[ActionStep],
  ctx: &mut ExecutionContext,
  events: &mpsc::Sender<RunnerEvent>,
) {
  for step in steps {
    let (display, warning) = initial(step, ctx);
    if let Some(warning) = warning {
      emit_warning(events, SETUP_STEP_ID, &warning).await;
    }
    ctx.set_step_display(&step.id, display);
  }
}

/// Retry an unevaluated name with the live contexts before the condition.
///
/// Returns the name to report and the warning to log once the step has
/// started. A failure keeps the earlier name, as upstream leaves the
/// timeline record untouched.
pub(crate) fn name_at_main(
  step: &ActionStep,
  ctx: &mut ExecutionContext,
) -> (String, Option<String>) {
  let mut display = ctx
    .step_display(&step.id)
    .cloned()
    .unwrap_or_else(|| initial(step, ctx).0);
  let mut warning = None;
  if !display.evaluated {
    match generate(step, ctx, &ctx.eval_context(), true) {
      Generated::Evaluated(name) => {
        display.current = or_fallback(mask(ctx, &name));
        display.evaluated = true;
      },
      Generated::Pending(_) => {},
      Generated::Failed(message) => warning = Some(message),
    }
  }
  let name = display.current.clone();
  ctx.set_step_display(&step.id, display);
  (name, warning)
}

/// The step's recorded names, generated quietly from job-start contexts
/// when it was never named (nested composite steps).
pub(crate) fn display_for(step: &ActionStep, ctx: &ExecutionContext) -> StepDisplay {
  ctx
    .step_display(&step.id)
    .cloned()
    .unwrap_or_else(|| initial(step, ctx).0)
}

/// Log a display-name failure as upstream's `##[warning]` line.
pub(crate) async fn emit_warning(events: &mpsc::Sender<RunnerEvent>, step_id: &str, warning: &str) {
  let event = RunnerEvent::Log {
    step_id: step_id.to_owned(),
    line: format!("##[warning]{warning}"),
    stream: LogStream::Stdout,
  };
  if events.send(event).await.is_err() {
    tracing::warn!(
      step_id,
      "event channel closed; display-name warning was dropped"
    );
  }
}

fn initial(step: &ActionStep, ctx: &ExecutionContext) -> (StepDisplay, Option<String>) {
  let mut eval = ctx.eval_context();
  eval
    .contexts
    .retain(|name, _| !STEP_CONTEXTS.contains(&name.as_str()));
  let (name, evaluated, warning) = match generate(step, ctx, &eval, false) {
    Generated::Evaluated(name) => (mask(ctx, &name), true, None),
    Generated::Pending(name) => (name, false, None),
    Generated::Failed(warning) => (String::new(), false, Some(warning)),
  };
  let name = or_fallback(name);
  let display = StepDisplay {
    initial: name.clone(),
    current: name,
    evaluated,
  };
  (display, warning)
}

fn generate(
  step: &ActionStep,
  ctx: &ExecutionContext,
  eval: &EvalContext,
  step_functions: bool,
) -> Generated {
  let (prefix, token) = match &step.display_name_token {
    Some(token) => ("", token.clone()),
    None => (RUN_PREFIX, source_token(step)),
  };
  match evaluate(&token, ctx, eval, step_functions) {
    Generated::Evaluated(text) => Generated::Evaluated(format_step_name(prefix, &text)),
    Generated::Pending(text) => Generated::Pending(format_step_name(prefix, &text)),
    Generated::Failed(message) => Generated::Failed(message),
  }
}

/// The token upstream names an unnamed step after: the script, the image,
/// or the repository reference.
fn source_token(step: &ActionStep) -> TemplateToken {
  if step.is_run_step() {
    return step
      .inputs
      .to_map()
      .remove("script")
      .unwrap_or_else(|| TemplateToken::literal(""));
  }
  let reference = &step.reference;
  let is_image = reference
    .ref_type
    .as_deref()
    .is_some_and(|kind| kind.eq_ignore_ascii_case("containerRegistry"));
  if is_image {
    return TemplateToken::literal(reference.image.as_deref().unwrap_or_default());
  }
  let name = reference.name.as_deref().unwrap_or_default();
  let path = match reference.path.as_deref().filter(|path| !path.is_empty()) {
    Some(path) if name.is_empty() => path.to_owned(),
    Some(path) => format!("/{path}"),
    None => String::new(),
  };
  let repository = match reference.git_ref.as_deref().filter(|r| !r.is_empty()) {
    Some(git_ref) => format!("{name}{path}@{git_ref}"),
    None => format!("{name}{path}"),
  };
  TemplateToken::literal(&repository)
}

fn evaluate(
  token: &TemplateToken,
  ctx: &ExecutionContext,
  eval: &EvalContext,
  step_functions: bool,
) -> Generated {
  match token.token_type {
    0 => Generated::Evaluated(token.lit.clone().unwrap_or_default()),
    1 => failed(token, ctx, "A sequence was not expected"),
    2 => failed(token, ctx, "A mapping was not expected"),
    3 => {
      let expression = token.expr.as_deref().unwrap_or_default();
      let found = match references(expression) {
        Ok(found) => found,
        Err(error) => return failed(token, ctx, &message(&error)),
      };
      let functions_known = found.functions.iter().all(|function| {
        WELL_KNOWN_FUNCTIONS
          .iter()
          .chain(
            step_functions
              .then_some(&STEP_FUNCTIONS)
              .into_iter()
              .flatten(),
          )
          .any(|known| known.eq_ignore_ascii_case(function))
      });
      let contexts_present = found.contexts.iter().all(|context| {
        eval
          .contexts
          .keys()
          .any(|present| present.eq_ignore_ascii_case(context))
      });
      if !(functions_known && contexts_present) {
        return Generated::Pending(display_string(expression));
      }
      match ctx.evaluate_with(eval, expression) {
        Ok(ExprValue::Array(_)) => failed(token, ctx, "A sequence was not expected"),
        Ok(ExprValue::Object(_)) => failed(token, ctx, "A mapping was not expected"),
        Ok(value) => Generated::Evaluated(value.coerce_to_string()),
        Err(error) => failed(token, ctx, &message(&error)),
      }
    },
    5 => {
      Generated::Evaluated(ExprValue::Bool(token.bool_val.unwrap_or_default()).coerce_to_string())
    },
    6 => {
      Generated::Evaluated(ExprValue::Number(token.num_val.unwrap_or_default()).coerce_to_string())
    },
    _ => Generated::Evaluated(String::new()),
  }
}

/// Upstream's warning: the token as written, then the positioned
/// `The template is not valid.` message.
fn failed(token: &TemplateToken, ctx: &ExecutionContext, message: &str) -> Generated {
  let written = match (token.token_type, &token.expr, &token.lit) {
    (3, Some(expression), _) => format!("${{{{ {expression} }}}}"),
    (_, _, Some(literal)) => literal.clone(),
    _ => String::new(),
  };
  let error = StepAttrError::Template {
    file: token.file,
    line: token.line,
    col: token.col,
    message: message.to_owned(),
  };
  Generated::Failed(format!(
    "Encountered an error when evaluating display name {written}. {}",
    error.render(ctx.file_table())
  ))
}

fn message(error: &RunnerError) -> String {
  if let RunnerError::Expression(message) = error {
    message.clone()
  } else {
    error.to_string()
  }
}

/// Upstream's `FormatStepName`: nothing stays nothing; otherwise the
/// prefix plus the first line after leading whitespace.
fn format_step_name(prefix: &str, text: &str) -> String {
  if text.is_empty() {
    String::new()
  } else {
    format!("{prefix}{}", first_line(text))
  }
}

fn or_fallback(name: String) -> String {
  if name.is_empty() {
    FALLBACK_NAME.to_owned()
  } else {
    name
  }
}

fn mask(ctx: &ExecutionContext, name: &str) -> String {
  match ctx.masker().lock() {
    Ok(masker) => masker.mask(name).into_owned(),
    Err(poisoned) => poisoned.into_inner().mask(name).into_owned(),
  }
}
