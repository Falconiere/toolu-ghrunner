//! Core built-in function implementations (contains, startsWith, etc.).

use shared::RunnerError;

use super::super::evaluator::{EvalContext, JobStatus};
use super::super::types::ExprValue;

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
  match name.to_ascii_lowercase().as_str() {
    "success" => Ok(ExprValue::Bool(ctx.job_status == JobStatus::Success)),
    "failure" => Ok(ExprValue::Bool(ctx.job_status == JobStatus::Failure)),
    "always" => Ok(ExprValue::Bool(true)),
    "cancelled" => Ok(ExprValue::Bool(ctx.job_status == JobStatus::Cancelled)),
    "contains" => fn_contains(args),
    "startswith" => fn_starts_with(args),
    "endswith" => fn_ends_with(args),
    "format" => fn_format(args),
    "join" => fn_join(args),
    "tojson" => super::json_convert::fn_to_json(args),
    "fromjson" => super::json_convert::fn_from_json(args),
    _ => Err(RunnerError::Expression(format!("unknown function: {name}"))),
  }
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
    ExprValue::String(haystack) => {
      let needle = item.coerce_to_string();
      Ok(ExprValue::Bool(
        haystack
          .to_ascii_lowercase()
          .contains(&needle.to_ascii_lowercase()),
      ))
    },
    ExprValue::Array(arr) => {
      let found = arr.iter().any(|el| el.loose_eq(item));
      Ok(ExprValue::Bool(found))
    },
    ExprValue::Null | ExprValue::Bool(_) | ExprValue::Number(_) | ExprValue::Object(_) => {
      Ok(ExprValue::Bool(false))
    },
  }
}

fn fn_starts_with(args: &[ExprValue]) -> Result<ExprValue, RunnerError> {
  let (a, b) = arg2("startsWith", args)?;
  let s = a.coerce_to_string().to_ascii_lowercase();
  let prefix = b.coerce_to_string().to_ascii_lowercase();
  Ok(ExprValue::Bool(s.starts_with(&prefix)))
}

fn fn_ends_with(args: &[ExprValue]) -> Result<ExprValue, RunnerError> {
  let (a, b) = arg2("endsWith", args)?;
  let s = a.coerce_to_string().to_ascii_lowercase();
  let suffix = b.coerce_to_string().to_ascii_lowercase();
  Ok(ExprValue::Bool(s.ends_with(&suffix)))
}

fn fn_format(args: &[ExprValue]) -> Result<ExprValue, RunnerError> {
  let template_val = args
    .first()
    .ok_or_else(|| RunnerError::Expression("format requires at least 1 argument".to_owned()))?;
  let template = template_val.coerce_to_string();
  let replacements = args.get(1..).unwrap_or_default();
  let mut result = String::with_capacity(template.len());
  let chars: Vec<char> = template.chars().collect();
  let mut i = 0;

  while i < chars.len() {
    let Some(&ch) = chars.get(i) else { break };
    if ch == '{' {
      if chars.get(i + 1).copied() == Some('{') {
        result.push('{');
        i += 2;
      } else {
        let (idx, end) = parse_format_index(&chars, i + 1)?;
        let val = replacements
          .get(idx)
          .map(ExprValue::coerce_to_string)
          .unwrap_or_default();
        result.push_str(&val);
        i = end + 1;
      }
    } else if ch == '}' && chars.get(i + 1).copied() == Some('}') {
      result.push('}');
      i += 2;
    } else {
      result.push(ch);
      i += 1;
    }
  }

  Ok(ExprValue::String(result))
}

fn parse_format_index(chars: &[char], start: usize) -> Result<(usize, usize), RunnerError> {
  let mut end = start;
  while end < chars.len() && chars.get(end).copied() != Some('}') {
    end += 1;
  }
  if end >= chars.len() {
    return Err(RunnerError::Expression(
      "unclosed { in format string".to_owned(),
    ));
  }
  let num_str: String = chars.get(start..end).unwrap_or_default().iter().collect();
  let idx = num_str
    .parse::<usize>()
    .map_err(|_err| RunnerError::Expression(format!("invalid format index: {num_str}")))?;
  Ok((idx, end))
}

fn fn_join(args: &[ExprValue]) -> Result<ExprValue, RunnerError> {
  if args.is_empty() || args.len() > 2 {
    return Err(RunnerError::Expression(
      "join expects 1-2 arguments".to_owned(),
    ));
  }
  let separator = args
    .get(1)
    .map(ExprValue::coerce_to_string)
    .unwrap_or_else(|| ",".to_owned());

  let first = args
    .first()
    .ok_or_else(|| RunnerError::Expression("join expects at least 1 argument".to_owned()))?;

  match first {
    ExprValue::Array(arr) => {
      let parts: Vec<String> = arr.iter().map(ExprValue::coerce_to_string).collect();
      Ok(ExprValue::String(parts.join(&separator)))
    },
    ExprValue::Null
    | ExprValue::Bool(_)
    | ExprValue::Number(_)
    | ExprValue::String(_)
    | ExprValue::Object(_) => Ok(ExprValue::String(first.coerce_to_string())),
  }
}
