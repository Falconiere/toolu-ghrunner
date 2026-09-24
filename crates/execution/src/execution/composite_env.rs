//! Composite action environment: environment building,
//! file command path management, and result types.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use shared::{Conclusion, RunnerConfig, RunnerError, RunnerEvent};
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::actions::manifest::{ActionDefinition, CompositeStep};
use super::actions::prefetch::ActionFetcher;
use super::composite_expr::{CompositeField, composite_eval_context, interpolate_composite_expr};
use super::context::{ExecutionContext, runner_temp_dir};
use super::file_commands::{parse_env_file, parse_output_file, parse_path_file, strip_blocked_env};
use super::handlers::node::input_env_key;

/// Bundled (read-only) parameters for composite action execution.
///
/// The mutable [`ExecutionContext`] is threaded separately so nested `uses:`
/// steps can borrow it mutably while this bundle stays shareable.
pub struct CompositeParams<'a> {
  /// The composite action's parsed manifest.
  pub manifest: &'a ActionDefinition,
  /// `with:` inputs the calling step passed to this composite action.
  pub step_inputs: &'a HashMap<String, String>,
  /// Sink for the job's event stream.
  pub events: &'a mpsc::Sender<RunnerEvent>,
  /// Job workspace root.
  pub workspace: &'a Path,
  /// The runner's configuration.
  pub config: &'a RunnerConfig,
  /// Id of the step that invoked this composite action.
  pub parent_step_id: &'a str,
  /// Directory the composite action was downloaded/checked out into.
  pub action_dir: &'a Path,
  /// Job-level cancellation token, threaded to nested `uses:` steps so a
  /// top-level cancel interrupts actions running inside a composite.
  pub cancel: &'a CancellationToken,
  /// Fixed top-level step deadline shared by every nested child.
  pub deadline: Option<Instant>,
  /// The job-scope HTTP client, threaded to nested `uses:` steps so their
  /// action resolution reuses the same connection pool.
  pub http: &'a reqwest::Client,
  /// The job-scope single-flight action fetcher, threaded to nested `uses:`
  /// steps so they join the same in-flight downloads as the top-level step
  /// and the job's prefetch task.
  pub fetcher: &'a ActionFetcher,
}

/// Result of composite execution including side effects (env/path changes).
pub struct CompositeResult {
  /// Overall outcome of the composite action's steps.
  pub conclusion: Conclusion,
  /// Outputs declared by the composite manifest and mapped from inner steps.
  pub outputs: HashMap<String, String>,
  /// `GITHUB_ENV` entries accumulated across the composite's steps.
  pub env_additions: HashMap<String, String>,
  /// `GITHUB_PATH` entries accumulated across the composite's steps.
  pub path_additions: Vec<String>,
}

/// Build the environment for a composite `run:` step: inherited job env plus
/// the step's own `env`, accumulated path additions, and runner paths.
///
/// `temp_dir` (the `RUNNER_TEMP` value below) is `runner_temp_dir` — the
/// same `data_dir/_temp` `set_runner_context` establishes for the job, so a
/// composite inline `run:` step never diverges from a top-level step's env
/// (B-004). The step's own `env:` values interpolate `${{ runner.* }}`
/// (incl. `runner.temp`) straight from `ctx` (B-005), so that source can't
/// drift from this env value either.
pub(super) fn build_step_env(
  params: &CompositeParams<'_>,
  ctx: &ExecutionContext,
  step: &CompositeStep,
  extra_env: &HashMap<String, String>,
  path_additions: &[String],
) -> Result<HashMap<String, String>, RunnerError> {
  let temp_dir = runner_temp_dir(&params.config.data_dir);
  let mut env = ctx.build_step_env(&HashMap::new());
  inherit_safe_process_env(ctx, &mut env);
  env.extend(extra_env.clone());
  add_composite_inputs(&mut env, params.step_inputs);
  add_composite_action_path(&mut env, params);
  interpolate_step_env(&mut env, params.step_inputs, &step.env, ctx)?;
  prepend_path_additions(&mut env, path_additions);
  add_runner_paths(&mut env, params.workspace, &temp_dir);
  translate_container_env(&mut env, ctx);
  Ok(env)
}

fn inherit_safe_process_env(ctx: &ExecutionContext, env: &mut HashMap<String, String>) {
  if ctx.job_container().is_none() {
    for (key, value) in super::context::safe_process_env_vars() {
      env.entry(key).or_insert(value);
    }
  }
}

fn add_composite_inputs(env: &mut HashMap<String, String>, inputs: &HashMap<String, String>) {
  for (key, value) in inputs {
    env.insert(input_env_key(key), value.clone());
  }
}

fn add_composite_action_path(env: &mut HashMap<String, String>, params: &CompositeParams<'_>) {
  env.insert(
    "GITHUB_ACTION_PATH".to_owned(),
    params.action_dir.to_string_lossy().into_owned(),
  );
}

fn interpolate_step_env(
  env: &mut HashMap<String, String>,
  inputs: &HashMap<String, String>,
  step_env: &HashMap<String, String>,
  ctx: &ExecutionContext,
) -> Result<(), RunnerError> {
  let eval_ctx = composite_eval_context(ctx, inputs, Some(env));
  for (key, value) in step_env {
    let interpolated = interpolate_composite_expr(value, &eval_ctx, CompositeField::Env)?;
    env.insert(key.clone(), interpolated);
  }
  Ok(())
}

fn add_runner_paths(env: &mut HashMap<String, String>, workspace: &Path, temp: &Path) {
  env.insert(
    "GITHUB_WORKSPACE".to_owned(),
    workspace.to_string_lossy().into_owned(),
  );
  env.insert(
    "RUNNER_TEMP".to_owned(),
    temp.to_string_lossy().into_owned(),
  );
}

fn translate_container_env(env: &mut HashMap<String, String>, ctx: &ExecutionContext) {
  if let Some(container) = ctx.job_container() {
    for (key, value) in env {
      *value = container.translate_env(key, value);
    }
  }
}

/// Prepend composite `GITHUB_PATH` additions ahead of the inherited `PATH`.
fn prepend_path_additions(env: &mut HashMap<String, String>, path_additions: &[String]) {
  if path_additions.is_empty() {
    return;
  }
  let existing = env.get("PATH").cloned().unwrap_or_default();
  let mut parts: Vec<&str> = path_additions.iter().map(String::as_str).collect();
  if !existing.is_empty() {
    parts.push(&existing);
  }
  env.insert("PATH".to_owned(), parts.join(":"));
}

/// Per-step `GITHUB_OUTPUT` / `GITHUB_ENV` / `GITHUB_PATH` file locations.
pub(super) struct FileCommandPaths {
  pub(super) output: PathBuf,
  pub(super) env: PathBuf,
  pub(super) path: PathBuf,
}

/// Create empty file-command files for a composite step under `temp_dir`.
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

/// Read back a step's file commands into outputs / env / path accumulators.
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
    // Mirror the top-level file-command read-back: strip the blocked
    // `NODE_OPTIONS` before folding, so a composite `run:` step can't set a
    // node preload for later composite node children.
    let mut parsed = parse_env_file(&content);
    strip_blocked_env(&mut parsed);
    extra_env.extend(parsed);
  }
  if let Ok(content) = std::fs::read_to_string(&files.path) {
    path_additions.extend(parse_path_file(&content));
  }
}
