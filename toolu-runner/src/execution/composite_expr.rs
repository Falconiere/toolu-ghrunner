use regex::Regex;

/// Interpolate `${{ ... }}` expressions in a composite step string.
///
/// Supported expressions:
/// - `inputs.NAME` — from the action's resolved inputs
/// - `steps.ID.outputs.KEY` — from previously completed composite steps
/// - `runner.os` / `runner.arch` / `runner.temp`
/// - `env.NAME` — from the current environment context
/// - Anything else resolves to an empty string.
pub fn interpolate_composite_expr(
  text: &str,
  inputs: &std::collections::HashMap<String, String>,
  step_outputs: &std::collections::HashMap<String, std::collections::HashMap<String, String>>,
  env_context: &std::collections::HashMap<String, String>,
  temp_dir: &std::path::Path,
) -> String {
  let Ok(re) = Regex::new(r"\$\{\{\s*(.*?)\s*\}\}") else {
    return text.to_owned();
  };

  re.replace_all(text, |caps: &regex::Captures<'_>| {
    let expr = caps.get(1).map(|m| m.as_str()).unwrap_or_default();
    resolve_expr(expr, inputs, step_outputs, env_context, temp_dir)
  })
  .into_owned()
}

fn resolve_expr(
  expr: &str,
  inputs: &std::collections::HashMap<String, String>,
  step_outputs: &std::collections::HashMap<String, std::collections::HashMap<String, String>>,
  env_context: &std::collections::HashMap<String, String>,
  temp_dir: &std::path::Path,
) -> String {
  let parts: Vec<&str> = expr.split('.').collect();

  match parts.first().copied() {
    Some("inputs") => resolve_input(&parts, inputs),
    Some("steps") => resolve_step_output(&parts, step_outputs),
    Some("runner") => resolve_runner(&parts, temp_dir),
    Some("env") => resolve_env(&parts, env_context),
    _ => String::new(),
  }
}

fn resolve_input(parts: &[&str], inputs: &std::collections::HashMap<String, String>) -> String {
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

fn resolve_step_output(
  parts: &[&str],
  step_outputs: &std::collections::HashMap<String, std::collections::HashMap<String, String>>,
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

fn resolve_runner(parts: &[&str], temp_dir: &std::path::Path) -> String {
  match parts.get(1).copied() {
    Some("os") => detect_os().to_owned(),
    Some("arch") => detect_arch().to_owned(),
    Some("temp") => temp_dir.to_string_lossy().into_owned(),
    _ => String::new(),
  }
}

fn resolve_env(parts: &[&str], env_context: &std::collections::HashMap<String, String>) -> String {
  let key = parts.get(1).copied().unwrap_or_default();
  env_context.get(key).cloned().unwrap_or_default()
}

fn detect_os() -> &'static str {
  if cfg!(target_os = "macos") {
    "macOS"
  } else if cfg!(target_os = "windows") {
    "Windows"
  } else {
    "Linux"
  }
}

fn detect_arch() -> &'static str {
  if cfg!(target_arch = "aarch64") {
    "ARM64"
  } else {
    "X64"
  }
}
