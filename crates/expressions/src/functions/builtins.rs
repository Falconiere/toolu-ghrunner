//! Core built-in function implementations (contains, startsWith, etc.).

use shared::RunnerError;

use super::super::evaluator::{EvalContext, JobStatus};
use super::super::types::{ExprValue, ordinal_upper};

/// Call a built-in GitHub Actions expression function.
///
/// Function name matching is case-insensitive.
///
/// # Errors
///
/// Returns `RunnerError::Expression` for unknown functions or wrong argument counts.
pub fn call_function(
  name: &str,
  args: &[ExprValue],
  ctx: &EvalContext,
) -> Result<ExprValue, RunnerError> {
  crate::validation::validate_arity(name, args.len())?;
  match name.to_ascii_lowercase().as_str() {
    "success" => Ok(ExprValue::Bool(ctx.job_status == JobStatus::Success)),
    "failure" => Ok(ExprValue::Bool(ctx.job_status == JobStatus::Failure)),
    "always" => Ok(ExprValue::Bool(true)),
    "cancelled" => Ok(ExprValue::Bool(ctx.job_status == JobStatus::Cancelled)),
    "contains" => fn_contains(args),
    "startswith" => fn_starts_with(args),
    "endswith" => fn_ends_with(args),
    "format" => fn_format(args),
    "case" => fn_case(args),
    "join" => fn_join(args),
    "tojson" => super::json_convert::fn_to_json(args),
    "fromjson" => super::json_convert::fn_from_json(args),
    "hashfiles" => fn_hash_files(args, ctx),
    _ => Err(RunnerError::Expression(format!("unknown function: {name}"))),
  }
}

/// `hashFiles(pattern, ...)` — SHA-256 over the workspace files it matches.
fn fn_hash_files(args: &[ExprValue], ctx: &EvalContext) -> Result<ExprValue, RunnerError> {
  if args.is_empty() {
    return Err(RunnerError::Expression(
      "hashFiles expects at least 1 argument".to_owned(),
    ));
  }
  let workspace = ctx.workspace.as_deref().ok_or_else(|| {
    RunnerError::Expression("hashFiles is unavailable outside a job workspace".to_owned())
  })?;
  let patterns: Vec<String> = args.iter().map(ExprValue::coerce_to_string).collect();
  super::hash::hash_files(workspace, &patterns).map(ExprValue::String)
}

fn arg2<'a>(
  name: &str,
  args: &'a [ExprValue],
) -> Result<(&'a ExprValue, &'a ExprValue), RunnerError> {
  let a = args
    .first()
    .ok_or_else(|| RunnerError::Expression(format!("{name} expects 2 args, got {}", args.len())))?;
  let b = args
    .get(1)
    .ok_or_else(|| RunnerError::Expression(format!("{name} expects 2 args, got {}", args.len())))?;
  if args.len() != 2 {
    return Err(RunnerError::Expression(format!(
      "{name} expects 2 args, got {}",
      args.len()
    )));
  }
  Ok((a, b))
}

fn fn_contains(args: &[ExprValue]) -> Result<ExprValue, RunnerError> {
  let (search, item) = arg2("contains", args)?;
  match search {
    ExprValue::String(_) | ExprValue::Null | ExprValue::Bool(_) | ExprValue::Number(_) => {
      if !item.is_primitive() {
        return Ok(ExprValue::Bool(false));
      }
      let haystack = ordinal_upper(&search.coerce_to_string());
      let needle = ordinal_upper(&item.coerce_to_string());
      Ok(ExprValue::Bool(
        needle.is_empty() || haystack.windows(needle.len()).any(|part| part == needle),
      ))
    },
    ExprValue::Array(arr) => {
      let found = arr.iter().any(|el| el.loose_eq(item));
      Ok(ExprValue::Bool(found))
    },
    ExprValue::Object(_) => Ok(ExprValue::Bool(false)),
  }
}

fn fn_starts_with(args: &[ExprValue]) -> Result<ExprValue, RunnerError> {
  let (a, b) = arg2("startsWith", args)?;
  if !a.is_primitive() || !b.is_primitive() {
    return Ok(ExprValue::Bool(false));
  }
  let s = ordinal_upper(&a.coerce_to_string());
  let prefix = ordinal_upper(&b.coerce_to_string());
  Ok(ExprValue::Bool(s.starts_with(&prefix)))
}

fn fn_ends_with(args: &[ExprValue]) -> Result<ExprValue, RunnerError> {
  let (a, b) = arg2("endsWith", args)?;
  if !a.is_primitive() || !b.is_primitive() {
    return Ok(ExprValue::Bool(false));
  }
  let s = ordinal_upper(&a.coerce_to_string());
  let suffix = ordinal_upper(&b.coerce_to_string());
  Ok(ExprValue::Bool(s.ends_with(&suffix)))
}

fn fn_format(args: &[ExprValue]) -> Result<ExprValue, RunnerError> {
  let template = args
    .first()
    .ok_or_else(|| RunnerError::Expression("format requires an argument".to_owned()))?
    .coerce_to_string();
  super::format::render(&template, args.len().saturating_sub(1), |index| {
    args
      .get(index + 1)
      .cloned()
      .ok_or_else(|| RunnerError::Expression("format argument out of range".to_owned()))
  })
}

fn fn_case(args: &[ExprValue]) -> Result<ExprValue, RunnerError> {
  if args.len().is_multiple_of(2) {
    return Err(RunnerError::Expression(
      "case requires an odd number of arguments".to_owned(),
    ));
  }
  for pair in args.chunks_exact(2) {
    match pair.first() {
      Some(ExprValue::Bool(true)) => {
        return pair
          .get(1)
          .cloned()
          .ok_or_else(|| RunnerError::Expression("case missing result".to_owned()));
      },
      Some(ExprValue::Bool(false)) => {},
      _ => {
        return Err(RunnerError::Expression(
          "case predicate must evaluate to a boolean".to_owned(),
        ));
      },
    }
  }
  args
    .last()
    .cloned()
    .ok_or_else(|| RunnerError::Expression("case missing default".to_owned()))
}

fn fn_join(args: &[ExprValue]) -> Result<ExprValue, RunnerError> {
  if args.is_empty() || args.len() > 2 {
    return Err(RunnerError::Expression(
      "join expects 1-2 arguments".to_owned(),
    ));
  }
  let separator = args
    .get(1)
    .filter(|value| value.is_primitive())
    .map_or_else(|| ",".to_owned(), ExprValue::coerce_to_string);

  let first = args
    .first()
    .ok_or_else(|| RunnerError::Expression("join expects at least 1 argument".to_owned()))?;

  match first {
    ExprValue::Array(arr) => {
      let parts: Vec<String> = arr.iter().map(ExprValue::coerce_to_string).collect();
      Ok(ExprValue::String(parts.join(&separator)))
    },
    ExprValue::Null | ExprValue::Bool(_) | ExprValue::Number(_) | ExprValue::String(_) => {
      Ok(ExprValue::String(first.coerce_to_string()))
    },
    ExprValue::Object(_) => Ok(ExprValue::String(String::new())),
  }
}
