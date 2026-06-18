//! Composite action environment: step skipping, environment building,
//! file command path management, and result types.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use shared::{Conclusion, RunnerConfig, RunnerError, RunnerEvent};
use tokio::sync::mpsc;

use super::actions::manifest::{ActionDefinition, CompositeStep};
use super::composite_expr::interpolate_composite_expr;
use super::context::ExecutionContext;
use super::file_commands::{parse_env_file, parse_output_file, parse_path_file};

/// Bundled parameters for composite action execution.
pub struct CompositeParams<'a> {
  pub manifest: &'a ActionDefinition,
  pub step_inputs: &'a HashMap<String, String>,
  pub ctx: &'a ExecutionContext,
  pub events: &'a mpsc::Sender<RunnerEvent>,
  pub workspace: &'a Path,
  pub config: &'a RunnerConfig,
  pub parent_step_id: &'a str,
  pub action_dir: &'a Path,
}

/// Result of composite execution including side effects (env/path changes).
pub struct CompositeResult {
  pub conclusion: Conclusion,
  pub env_additions: HashMap<String, String>,
  pub path_additions: Vec<String>,
}

pub(super) fn should_skip_step(step: &CompositeStep) -> bool {
  let Some(cond) = &step.condition else {
    return false;
  };
  let trimmed = cond.trim();
  if trimmed.eq_ignore_ascii_case("false") {
    return true;
  }
  // runner.os == 'Windows' on non-Windows
  if trimmed.contains("runner.os == 'Windows'") || trimmed.contains("runner.os == \"Windows\"") {
    return std::env::consts::OS != "windows";
  }
  // runner.os != 'Windows' on Windows
  if trimmed.contains("runner.os != 'Windows'") || trimmed.contains("runner.os != \"Windows\"") {
    return std::env::consts::OS == "windows";
  }
  false
}

pub(super) fn build_step_env(
  params: &CompositeParams<'_>,
  step: &CompositeStep,
  extra_env: &HashMap<String, String>,
  path_additions: &[String],
) -> HashMap<String, String> {
  let temp_dir = params.config.data_dir.join("tmp");
  let mut env = params.ctx.build_step_env(&HashMap::new());

  // Inherit system env for PATH, HOME, etc.
  for (k, v) in std::env::vars() {
    env.entry(k).or_insert(v);
  }

  env.extend(extra_env.clone());

  // INPUT_* vars from step inputs
  for (k, v) in params.step_inputs {
    env.insert(format!("INPUT_{}", k.to_uppercase()), v.clone());
  }

  env.insert(
    "GITHUB_ACTION_PATH".to_owned(),
    params.action_dir.to_string_lossy().into_owned(),
  );

  // Step-level env (interpolated)
  for (k, v) in &step.env {
    let interpolated =
      interpolate_composite_expr(v, params.step_inputs, &HashMap::new(), &env, &temp_dir);
    env.insert(k.clone(), interpolated);
  }

  // Prepend path additions
  if !path_additions.is_empty() {
    let existing = env.get("PATH").cloned().unwrap_or_default();
    let mut parts: Vec<&str> = path_additions.iter().map(String::as_str).collect();
    if !existing.is_empty() {
      parts.push(&existing);
    }
    env.insert("PATH".to_owned(), parts.join(":"));
  }

  env.insert(
    "GITHUB_WORKSPACE".to_owned(),
    params.workspace.to_string_lossy().into_owned(),
  );
  env.insert(
    "RUNNER_TEMP".to_owned(),
    temp_dir.to_string_lossy().into_owned(),
  );

  env
}

pub(super) struct FileCommandPaths {
  pub(super) output: PathBuf,
  pub(super) env: PathBuf,
  pub(super) path: PathBuf,
}

pub(super) fn create_file_command_files(
  temp_dir: &Path,
  step_id: &str,
) -> Result<FileCommandPaths, RunnerError> {
  let paths = FileCommandPaths {
    output: temp_dir.join(format!("composite_output_{step_id}")),
    env: temp_dir.join(format!("composite_env_{step_id}")),
    path: temp_dir.join(format!("composite_path_{step_id}")),
  };
  std::fs::write(&paths.output, "")?;
  std::fs::write(&paths.env, "")?;
  std::fs::write(&paths.path, "")?;
  Ok(paths)
}

pub(super) fn process_file_commands(
  files: &FileCommandPaths,
  step_id: &str,
  step_outputs: &mut HashMap<String, HashMap<String, String>>,
  extra_env: &mut HashMap<String, String>,
  path_additions: &mut Vec<String>,
) {
  if let Ok(content) = std::fs::read_to_string(&files.output) {
    let outputs = parse_output_file(&content);
    if !outputs.is_empty() {
      step_outputs.insert(step_id.to_owned(), outputs);
    }
  }
  if let Ok(content) = std::fs::read_to_string(&files.env) {
    extra_env.extend(parse_env_file(&content));
  }
  if let Ok(content) = std::fs::read_to_string(&files.path) {
    path_additions.extend(parse_path_file(&content));
  }
}
