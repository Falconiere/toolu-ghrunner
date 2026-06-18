//! Download and execute GitHub Actions (`uses:` steps).

use std::path::Path;

use shared::{ActionStep, Conclusion, RunnerConfig, RunnerError, RunnerEvent};
use tokio::sync::mpsc;

use super::action_support::{
  build_composite_inputs, build_node_env, emit_action_header, emit_log, read_manifest,
  resolve_action_dir,
};
use super::actions::downloader::{action_cache_dir, download_and_extract_action, is_action_cached};
use super::actions::manifest::RunsUsing;
use super::actions::resolver::{ActionRefKind, parse_action_ref};
use super::composite_exec::{CompositeParams, execute_composite_action};
use super::context::ExecutionContext;
use super::handlers::node_exec::{NodeExecParams, execute_node_action};
use crate::node::runtime::ensure_node_runtime;

/// Resolved action ready for execution.
struct ResolvedStep {
  client: reqwest::Client,
  action_dir: std::path::PathBuf,
  manifest: super::actions::manifest::ActionDefinition,
}

/// Execute an action step end-to-end: resolve -> download -> parse manifest -> run.
///
/// # Errors
///
/// Returns `RunnerError` on download, manifest parse, or execution failure.
pub async fn execute_action(
  step: &ActionStep,
  ctx: &mut ExecutionContext,
  events: &mpsc::Sender<RunnerEvent>,
  workspace: &Path,
  config: &RunnerConfig,
) -> Result<Conclusion, RunnerError> {
  let resolved = resolve_and_download(step, config, events).await?;
  dispatch_action(step, ctx, events, workspace, config, &resolved).await
}

/// # Errors
///
/// Returns `RunnerError` on resolution, download, or manifest parse failure.
async fn resolve_and_download(
  step: &ActionStep,
  config: &RunnerConfig,
  events: &mpsc::Sender<RunnerEvent>,
) -> Result<ResolvedStep, RunnerError> {
  let uses = step
    .reference
    .name
    .as_deref()
    .or(step.reference.image.as_deref())
    .unwrap_or("");
  let git_ref = step.reference.git_ref.as_deref().unwrap_or("");
  let uses_full = format!("{uses}@{git_ref}");

  let action_ref = parse_action_ref(&uses_full)?;

  if action_ref.kind == ActionRefKind::Local {
    return Err(RunnerError::ActionResolution(
      "local actions not yet supported".to_owned(),
    ));
  }

  let client = reqwest::Client::new();
  let cache_key = action_ref.cache_key();
  let cache_dir = action_cache_dir(&config.data_dir, &cache_key);

  if !is_action_cached(&cache_dir) {
    let tarball_url = action_ref.tarball_url("https://api.github.com");
    emit_log(events, &step.id, &format!("Downloading {uses_full}...")).await;
    download_and_extract_action(&client, &tarball_url, None, &cache_dir).await?;
  }

  let action_dir = resolve_action_dir(&cache_dir, &action_ref.subpath);
  let manifest = read_manifest(&action_dir)?;
  emit_action_header(step, &uses_full, events).await;

  Ok(ResolvedStep {
    client,
    action_dir,
    manifest,
  })
}

async fn dispatch_action(
  step: &ActionStep,
  ctx: &mut ExecutionContext,
  events: &mpsc::Sender<RunnerEvent>,
  workspace: &Path,
  config: &RunnerConfig,
  resolved: &ResolvedStep,
) -> Result<Conclusion, RunnerError> {
  let ResolvedStep {
    client,
    action_dir,
    manifest,
  } = resolved;

  match manifest.runs.using {
    RunsUsing::Node { major } => {
      let node_binary = ensure_node_runtime(client, &config.data_dir, major).await?;
      let env = build_node_env(step, ctx, manifest, action_dir, workspace, config);
      let script = action_dir.join(manifest.runs.main.as_deref().unwrap_or("dist/index.js"));
      if !script.exists() {
        return Err(RunnerError::ActionManifest(format!(
          "main script not found: {}",
          script.display()
        )));
      }
      emit_log(events, &step.id, "##[endgroup]").await;
      let node_params = NodeExecParams {
        node_binary: &node_binary,
        script_path: &script,
        env: &env,
        working_dir: workspace,
        step_id: &step.id,
        cgroup_path: ctx.cgroup_path(),
      };
      execute_node_action(&node_params, events).await
    },
    RunsUsing::Composite => {
      let step_inputs = build_composite_inputs(step, manifest);
      emit_log(events, &step.id, "##[endgroup]").await;
      let params = CompositeParams {
        manifest,
        step_inputs: &step_inputs,
        ctx,
        events,
        workspace,
        config,
        parent_step_id: &step.id,
        action_dir,
      };
      let result = execute_composite_action(&params).await?;
      // Propagate env/path changes to parent context
      for (k, v) in &result.env_additions {
        ctx.set_env(k, v);
      }
      for p in &result.path_additions {
        ctx.prepend_path(p);
      }
      Ok(result.conclusion)
    },
    RunsUsing::Docker => {
      emit_log(events, &step.id, "  (docker actions not yet supported)").await;
      Err(RunnerError::ActionResolution(
        "docker actions not yet supported".into(),
      ))
    },
  }
}
