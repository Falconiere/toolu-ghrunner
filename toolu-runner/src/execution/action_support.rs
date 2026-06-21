//! Action resolution: environment building, manifest reading, input merging,
//! and logging for action steps.

use std::collections::HashMap;
use std::path::Path;

use shared::{ActionStep, LogStream, RunnerConfig, RunnerError, RunnerEvent};
use tokio::sync::mpsc;

use super::actions::manifest::ActionDefinition;
use super::context::ExecutionContext;
use super::handlers::node::build_action_env;

/// Build the env map for a Node.js action stage (inputs, `STATE_*`, paths).
pub(super) fn build_node_env(
  step: &ActionStep,
  ctx: &ExecutionContext,
  manifest: &ActionDefinition,
  action_dir: &Path,
  workspace: &Path,
  config: &RunnerConfig,
) -> HashMap<String, String> {
  let step_inputs = collect_step_inputs(step);

  // The step's `save-state` values surface as `STATE_*` to its own
  // pre/main/post stages (keyed by step id), so post can read what main saved.
  let state = ctx.step_state(&step.id);
  let mut env = ctx.build_step_env(&HashMap::new());
  env.extend(build_action_env(
    manifest,
    &step_inputs,
    &action_dir.to_string_lossy(),
    &state,
  ));

  // Interpolate ${{ ... }} expressions in INPUT_* values (action.yml defaults)
  for value in env.values_mut() {
    if value.contains("${{")
      && let Ok(interpolated) = ctx.interpolate_string(value)
    {
      *value = interpolated;
    }
  }
  apply_runner_paths(&mut env, workspace, config);
  env
}

/// Collect a step's `with:` inputs as plain string key/values.
fn collect_step_inputs(step: &ActionStep) -> HashMap<String, String> {
  step
    .inputs
    .to_map()
    .into_iter()
    .map(|(k, v)| {
      (
        k,
        v.to_string_value()
          .map(ToOwned::to_owned)
          .unwrap_or_default(),
      )
    })
    .collect()
}

/// Inject `GITHUB_WORKSPACE` / `RUNNER_TEMP` / `RUNNER_TOOL_CACHE`.
fn apply_runner_paths(env: &mut HashMap<String, String>, workspace: &Path, config: &RunnerConfig) {
  env.insert(
    "GITHUB_WORKSPACE".to_owned(),
    workspace.to_string_lossy().into_owned(),
  );
  env.insert(
    "RUNNER_TEMP".to_owned(),
    config.data_dir.join("tmp").to_string_lossy().into_owned(),
  );
  env.insert(
    "RUNNER_TOOL_CACHE".to_owned(),
    config
      .data_dir
      .join("tool_cache")
      .to_string_lossy()
      .into_owned(),
  );
}

/// Merge a composite step's `with:` inputs with the manifest's input defaults.
pub(super) fn build_composite_inputs(
  step: &ActionStep,
  manifest: &ActionDefinition,
) -> HashMap<String, String> {
  let user_inputs: HashMap<_, _> = step
    .inputs
    .to_map()
    .into_iter()
    .map(|(k, v)| {
      (
        k,
        v.to_string_value()
          .map(ToOwned::to_owned)
          .unwrap_or_default(),
      )
    })
    .collect();

  let mut result = HashMap::new();
  for (name, input_def) in &manifest.inputs {
    let value = user_inputs
      .get(name)
      .cloned()
      .or_else(|| input_def.default.clone())
      .unwrap_or_default();
    result.insert(name.clone(), value);
  }
  // Also include any user inputs not declared in manifest
  for (k, v) in &user_inputs {
    result.entry(k.clone()).or_insert_with(|| v.clone());
  }
  result
}

/// Resolve the action directory inside its cache dir, honoring a subpath.
pub(super) fn resolve_action_dir(cache_dir: &Path, subpath: &Option<String>) -> std::path::PathBuf {
  match subpath {
    Some(p) if !p.is_empty() => cache_dir.join(p),
    _ => cache_dir.to_path_buf(),
  }
}

/// Read and parse `action.yml`/`action.yaml` from an action directory.
pub(super) fn read_manifest(action_dir: &Path) -> Result<ActionDefinition, RunnerError> {
  let yml_path = action_dir.join("action.yml");
  let yaml_path = action_dir.join("action.yaml");

  let manifest_path = if yml_path.exists() {
    yml_path
  } else if yaml_path.exists() {
    yaml_path
  } else {
    return Err(RunnerError::ActionManifest(format!(
      "no action.yml or action.yaml in {}",
      action_dir.display()
    )));
  };

  let content = std::fs::read_to_string(&manifest_path)
    .map_err(|e| RunnerError::ActionManifest(format!("read {}: {e}", manifest_path.display())))?;

  super::actions::manifest::parse_action_manifest(&content)
}

pub(super) async fn emit_action_header(
  step: &ActionStep,
  uses_full: &str,
  events: &mpsc::Sender<RunnerEvent>,
) {
  emit_log(events, &step.id, &format!("##[group]Run {uses_full}")).await;
  emit_log(events, &step.id, &format!("  uses: {uses_full}")).await;
  let input_map = step.inputs.to_map();
  if !input_map.is_empty() {
    emit_log(events, &step.id, "  with:").await;
    for (k, v) in &input_map {
      let value = v
        .to_string_value()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| "<unrenderable>".to_owned());
      emit_log(events, &step.id, &format!("    {k}: {value}")).await;
    }
  }
}

pub(super) async fn emit_log(events: &mpsc::Sender<RunnerEvent>, step_id: &str, line: &str) {
  let _ = events
    .send(RunnerEvent::Log {
      step_id: step_id.to_owned(),
      line: line.to_owned(),
      stream: LogStream::Stdout,
    })
    .await;
}
