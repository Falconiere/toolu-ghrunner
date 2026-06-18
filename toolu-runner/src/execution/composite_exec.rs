//! Composite action executor.
//!
//! Runs each step in a composite action's `steps:` array as a shell subprocess,
//! managing `GITHUB_OUTPUT`, `GITHUB_ENV`, and `GITHUB_PATH` file commands
//! between steps.

use std::collections::HashMap;

use shared::{Conclusion, LogStream, RunnerError, RunnerEvent};
use tokio::sync::mpsc;

pub use super::composite_env::{CompositeParams, CompositeResult};
use super::composite_env::{
  build_step_env, create_file_command_files, process_file_commands, should_skip_step,
};
use super::composite_expr::interpolate_composite_expr;
use super::composite_shell::{ShellScriptParams, run_shell_script};

/// Execute a composite action's steps sequentially.
///
/// # Errors
///
/// Returns `RunnerError::StepExecution` on subprocess spawn failures.
pub async fn execute_composite_action(
  params: &CompositeParams<'_>,
) -> Result<CompositeResult, RunnerError> {
  let temp_dir = params.config.data_dir.join("tmp");
  std::fs::create_dir_all(&temp_dir)?;

  let mut step_outputs: HashMap<String, HashMap<String, String>> = HashMap::new();
  let mut extra_env: HashMap<String, String> = HashMap::new();
  let mut path_additions: Vec<String> = Vec::new();

  for (idx, step) in params.manifest.runs.steps.iter().enumerate() {
    if should_skip_step(step) {
      continue;
    }

    let Some(script) = &step.run else { continue };

    let step_id = step
      .id
      .clone()
      .unwrap_or_else(|| format!("__composite_{idx}"));
    let step_name = step
      .name
      .as_deref()
      .unwrap_or_else(|| script.lines().next().unwrap_or("(composite step)"));

    emit_log(
      params.events,
      params.parent_step_id,
      &format!("##[group]Run {step_name}"),
    )
    .await;

    let env = build_step_env(params, step, &extra_env, &path_additions);
    let file_paths = create_file_command_files(&temp_dir, &step_id)?;
    let full_env = merge_file_command_env(&env, &file_paths);

    let interpolated =
      interpolate_composite_expr(script, params.step_inputs, &step_outputs, &env, &temp_dir);

    let shell = step.shell.as_deref().unwrap_or("bash");
    let shell_params = ShellScriptParams {
      shell,
      script: &interpolated,
      env: &full_env,
      working_dir: params.workspace,
      log_step_id: params.parent_step_id,
      cgroup_path: params.ctx.cgroup_path(),
    };
    let conclusion = run_shell_script(&shell_params, params.events).await?;

    emit_log(params.events, params.parent_step_id, "##[endgroup]").await;

    process_file_commands(
      &file_paths,
      &step_id,
      &mut step_outputs,
      &mut extra_env,
      &mut path_additions,
    );

    if conclusion == Conclusion::Failure && !step.continue_on_error {
      return Ok(CompositeResult {
        conclusion: Conclusion::Failure,
        env_additions: extra_env,
        path_additions,
      });
    }
  }

  Ok(CompositeResult {
    conclusion: Conclusion::Success,
    env_additions: extra_env,
    path_additions,
  })
}

fn merge_file_command_env(
  base: &HashMap<String, String>,
  files: &super::composite_env::FileCommandPaths,
) -> HashMap<String, String> {
  let mut env = base.clone();
  env.insert(
    "GITHUB_OUTPUT".to_owned(),
    files.output.to_string_lossy().into_owned(),
  );
  env.insert(
    "GITHUB_ENV".to_owned(),
    files.env.to_string_lossy().into_owned(),
  );
  env.insert(
    "GITHUB_PATH".to_owned(),
    files.path.to_string_lossy().into_owned(),
  );
  env
}

async fn emit_log(events: &mpsc::Sender<RunnerEvent>, step_id: &str, line: &str) {
  let _ = events
    .send(RunnerEvent::Log {
      step_id: step_id.to_owned(),
      line: line.to_owned(),
      stream: LogStream::Stdout,
    })
    .await;
}
