//! Container action stage environment, arguments and normal workflow commands.

use std::collections::HashMap;
use std::path::Path;

use shared::{ActionStep, Conclusion, RunnerConfig, RunnerError, RunnerEvent};
use tokio::sync::mpsc;

use super::action_support::{build_composite_inputs, build_resolved_action_env};
use super::actions::manifest::ActionDefinition;
use super::command_dispatch::stream_dispatch_stdout;
use super::context::ExecutionContext;
use super::file_commands::{FileCommandManager, create_file_command_dir};
use super::handlers::node::input_env_key;
use super::step_env::apply_file_commands_and_merge_outputs;
use super::step_timeout::StepBounds;
use crate::docker::action_container::{ActionContainer, ActionContainerParams};
use crate::docker::action_mounts::action_path_to_host;
use expressions::types::ExprValue;

/// Inputs shared by the pre, main and post stages of one Docker action.
#[derive(Clone, Copy)]
pub(super) struct DockerStage<'a> {
  pub step: &'a ActionStep,
  pub events: &'a mpsc::Sender<RunnerEvent>,
  pub workspace: &'a Path,
  pub config: &'a RunnerConfig,
  pub action_dir: &'a Path,
  pub manifest: &'a ActionDefinition,
  pub bounds: &'a StepBounds,
  pub stage: &'a str,
  pub log_step_id: &'a str,
  pub inputs: Option<&'a HashMap<String, String>>,
}

/// Execute one prepared image and apply commands in the originating action scope.
pub(super) async fn run_docker_stage(
  s: &DockerStage<'_>,
  ctx: &mut ExecutionContext,
  runtime: &ActionContainer,
  image: &str,
) -> Result<(Conclusion, HashMap<String, String>), RunnerError> {
  let resolved_inputs;
  let inputs = if let Some(inputs) = s.inputs {
    inputs
  } else {
    resolved_inputs = build_composite_inputs(s.step, s.manifest, ctx)?;
    &resolved_inputs
  };
  let (mut env, args, entrypoint) = stage_values(s, ctx, inputs)?;
  let (_, path_additions) = ctx.docker_action_path();
  let tmp = s.config.data_dir.join("tmp");
  create_file_command_dir(&tmp).await?;
  let (commands, command_env) = FileCommandManager::create(&tmp).await?;
  env.extend(command_env);
  let network = ctx
    .evaluate_expression("job.container.network")?
    .coerce_to_string();
  let params = ActionContainerParams {
    image,
    entrypoint: entrypoint.as_deref(),
    args: args.as_deref(),
    env: &env,
    path_additions: &path_additions,
    config: s.config,
    workspace: s.workspace,
    action_dir: s.action_dir,
    network: (!network.is_empty()).then_some(network.as_str()),
    step_id: s.log_step_id,
    timeout: s.bounds.remaining_timeout(),
    cancel: &s.bounds.cancel,
  };
  execute_stage(s, ctx, runtime, &params, &commands).await
}

async fn execute_stage(
  s: &DockerStage<'_>,
  ctx: &mut ExecutionContext,
  runtime: &ActionContainer,
  params: &ActionContainerParams<'_>,
  commands: &FileCommandManager,
) -> Result<(Conclusion, HashMap<String, String>), RunnerError> {
  let (tx, mut rx) = mpsc::channel(256);
  let name = (s.stage == "main")
    .then(|| s.step.expression_name())
    .flatten();
  let process = runtime.run(params, s.events, tx);
  let dispatch = stream_dispatch_stdout(&s.step.id, s.log_step_id, name, &mut rx, ctx, s.events);
  let (result, stdout_outputs) = tokio::join!(process, dispatch);
  // Read commands even on a daemon error after the action wrote them.
  translate_path_file(s, commands).await?;
  let outputs = apply_file_commands_and_merge_outputs(
    &s.step.id,
    name,
    Some(&s.step.id),
    stdout_outputs,
    commands,
    ctx,
  )
  .await;
  Ok((result?, outputs))
}

type StageValues = (HashMap<String, String>, Option<Vec<String>>, Option<String>);

fn stage_values(
  s: &DockerStage<'_>,
  ctx: &ExecutionContext,
  inputs: &HashMap<String, String>,
) -> Result<StageValues, RunnerError> {
  let mut env = build_resolved_action_env(
    (s.step, inputs),
    ctx,
    s.manifest,
    s.action_dir,
    s.workspace,
    s.config,
  );
  let (explicit_path, _) = ctx.docker_action_path();
  env.remove("PATH");
  if let Some(path) = explicit_path {
    env.insert("PATH".to_owned(), path);
  }
  // Docker actions expose all supplied inputs, including legacy args/entrypoint.
  for (key, value) in inputs {
    env.insert(input_env_key(key), value.clone());
  }
  let eval = action_eval(ctx, inputs);
  for (key, value) in &s.manifest.runs.env {
    let value = ctx.interpolate_with(&eval, value)?;
    env.entry(key.clone()).or_insert(value);
  }
  let args = if let Some(args) = &s.manifest.runs.args {
    Some(
      args
        .iter()
        .map(|arg| ctx.interpolate_with(&eval, arg))
        .collect::<Result<_, _>>()?,
    )
  } else if let Some(args) = input(inputs, "args") {
    Some(shlex::split(args).ok_or_else(|| {
      RunnerError::ActionManifest("invalid quoting in Docker action args".to_owned())
    })?)
  } else {
    None
  };
  Ok((env, args, stage_entrypoint(s, inputs)))
}

fn stage_entrypoint(s: &DockerStage<'_>, inputs: &HashMap<String, String>) -> Option<String> {
  match s.stage {
    "pre" => s.manifest.runs.pre_entrypoint.as_deref(),
    "post" => s.manifest.runs.post_entrypoint.as_deref(),
    _ => s
      .manifest
      .runs
      .entrypoint
      .as_deref()
      .filter(|v| !v.is_empty())
      .or_else(|| input(inputs, "entrypoint")),
  }
  .filter(|value| !value.is_empty())
  .map(str::to_owned)
}

/// Add this action's resolved inputs while retaining live job status.
pub(super) fn action_eval(
  ctx: &ExecutionContext,
  inputs: &HashMap<String, String>,
) -> expressions::evaluator::EvalContext {
  let mut eval = ctx.eval_context();
  eval.contexts.insert(
    "inputs".to_owned(),
    ExprValue::Object(
      inputs
        .iter()
        .map(|(key, value)| (key.clone(), ExprValue::String(value.clone())))
        .collect(),
    ),
  );
  eval
}

fn input<'a>(inputs: &'a HashMap<String, String>, name: &str) -> Option<&'a str> {
  inputs
    .iter()
    .find(|(key, _)| key.eq_ignore_ascii_case(name))
    .map(|(_, value)| value.as_str())
}

async fn translate_path_file(
  s: &DockerStage<'_>,
  commands: &FileCommandManager,
) -> Result<(), RunnerError> {
  let contents = tokio::fs::read_to_string(&commands.path_path).await?;
  let paths = contents
    .lines()
    .map(|line| {
      action_path_to_host(
        s.config,
        s.workspace,
        s.action_dir,
        line.trim_end_matches('\r'),
      )
      .to_string_lossy()
      .into_owned()
    })
    .collect::<Vec<_>>()
    .join("\n");
  tokio::fs::write(&commands.path_path, paths).await?;
  Ok(())
}
