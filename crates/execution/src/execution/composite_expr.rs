use regex::Regex;

use super::context::ExecutionContext;

/// Interpolate `${{ ... }}` expressions in a composite step string.
///
/// Supported expressions:
/// - `inputs.NAME` — from the action's resolved inputs
/// - `steps.ID.outputs.KEY` — from previously completed composite steps
/// - `runner.*` — read back from `ctx`, same as the real evaluator (B-005);
///   see [`resolve_runner`] for the exact semantics
/// - `env.NAME` — from the current environment context
/// - Anything else resolves to an empty string.
pub fn interpolate_composite_expr<S: ::std::hash::BuildHasher>(
  text: &str,
  inputs: &std::collections::HashMap<String, String, S>,
  step_outputs: &std::collections::HashMap<String, std::collections::HashMap<String, String, S>, S>,
  env_context: &std::collections::HashMap<String, String, S>,
  ctx: &ExecutionContext,
) -> String {
  let Ok(re) = Regex::new(r"\$\{\{\s*(.*?)\s*\}\}") else {
    return text.to_owned();
  };

  re.replace_all(text, |caps: &regex::Captures<'_>| {
    let expr = caps.get(1).map(|m| m.as_str()).unwrap_or_default();
    resolve_expr(expr, inputs, step_outputs, env_context, ctx)
  })
  .into_owned()
}

fn resolve_expr<S: ::std::hash::BuildHasher>(
  expr: &str,
  inputs: &std::collections::HashMap<String, String, S>,
  step_outputs: &std::collections::HashMap<String, std::collections::HashMap<String, String, S>, S>,
  env_context: &std::collections::HashMap<String, String, S>,
  ctx: &ExecutionContext,
) -> String {
  let parts: Vec<&str> = expr.split('.').collect();

  match parts.first().copied() {
    Some("inputs") => resolve_input(&parts, inputs),
    Some("steps") => resolve_step_output(&parts, step_outputs),
    Some("runner") => resolve_runner(&parts, ctx),
    Some("env") => resolve_env(&parts, env_context),
    _ => String::new(),
  }
}

fn resolve_input<S: ::std::hash::BuildHasher>(
  parts: &[&str],
  inputs: &std::collections::HashMap<String, String, S>,
) -> String {
  let key = parts.get(1).copied().unwrap_or_default();
  // Try exact match first, then case-insensitive
  if let Some(val) = inputs.get(key) {
    return val.clone();
  }
  for (k, v) in inputs {
    if k.eq_ignore_ascii_case(key) {
      return v.clone();
    }
  }
  String::new()
}

fn resolve_step_output<S: ::std::hash::BuildHasher>(
  parts: &[&str],
  step_outputs: &std::collections::HashMap<String, std::collections::HashMap<String, String, S>, S>,
) -> String {
  // steps.ID.outputs.KEY
  if parts.len() >= 4 && parts.get(2).copied() == Some("outputs") {
    let step_id = parts.get(1).copied().unwrap_or_default();
    let key = parts.get(3).copied().unwrap_or_default();
    return step_outputs
      .get(step_id)
      .and_then(|out| out.get(key))
      .cloned()
      .unwrap_or_default();
  }
  String::new()
}

/// Resolve `runner.<field>` by reading back [`ExecutionContext::runner_value`]
/// — the single source [`ExecutionContext::set_runner_context`] establishes
/// and the real evaluator's `runner.*` also reads (B-005). An absent key
/// (unknown field, or `debug` when step-debug is off) resolves to `""`.
fn resolve_runner(parts: &[&str], ctx: &ExecutionContext) -> String {
  let key = parts.get(1).copied().unwrap_or_default();
  ctx.runner_value(key).unwrap_or_default().to_owned()
}

fn resolve_env<S: ::std::hash::BuildHasher>(
  parts: &[&str],
  env_context: &std::collections::HashMap<String, String, S>,
) -> String {
  let key = parts.get(1).copied().unwrap_or_default();
  env_context.get(key).cloned().unwrap_or_default()
}
