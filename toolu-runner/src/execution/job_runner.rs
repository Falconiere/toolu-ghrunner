use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use shared::{AgentJobRequestMessage, Conclusion, RunnerConfig, RunnerError, RunnerEvent};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::info;

use super::cache::backend::LocalDiskBackend;
use super::cache::service::CacheService;
use super::context::ExecutionContext;
use super::expressions::context_data::pipeline_data_to_expr_value;
use super::secret_masker::SecretMasker;
use super::steps_runner::run_steps;

/// Default cache size limit: 10 GB.
const DEFAULT_CACHE_MAX_BYTES: u64 = 10 * 1024 * 1024 * 1024;

/// Execute a complete job from an `AgentJobRequestMessage`.
///
/// Starts a local cache service before job steps and shuts it down on completion.
///
/// # Errors
///
/// Returns `RunnerError` if workspace creation, cache service startup, or execution fails.
pub async fn run_job(
  msg: AgentJobRequestMessage,
  config: &RunnerConfig,
  cancel: CancellationToken,
  events: mpsc::Sender<RunnerEvent>,
  masker: Arc<Mutex<SecretMasker>>,
) -> Result<(), RunnerError> {
  let workspace = config.workspace_root.join(&msg.job_id);
  std::fs::create_dir_all(&workspace)?;
  std::fs::create_dir_all(&config.data_dir)?;

  let (cache_service, cache_token) = start_cache_service(config).await?;
  info!(
    url = cache_service.base_url(),
    "cache service started for job"
  );

  let mut ctx = build_context(&msg, config, masker);
  ctx.set_env("ACTIONS_CACHE_URL", cache_service.base_url());
  ctx.set_env("ACTIONS_RUNTIME_TOKEN", &cache_token);

  // Write event.json outside workspace (checkout wipes workspace contents).
  let event_path = write_event_json(&config.data_dir, &msg.job_id, &ctx)?;
  ctx.set_env("GITHUB_EVENT_PATH", &event_path);

  // Inject W3C trace context for distributed trace correlation.
  let trace_id = uuid::Uuid::new_v4().to_string().replace('-', "");
  let span_id = &trace_id[..16];
  ctx.set_env("TRACEPARENT", &format!("00-{trace_id}-{span_id}-01"));
  ctx.set_env("TRACESTATE", "toolu=true");

  emit_job_started(&events, &msg.job_id, &msg.job_display_name).await;

  let conclusion = run_steps(&msg.steps, &mut ctx, &events, cancel, &workspace, config).await?;

  cache_service.shutdown().await;
  emit_job_completed(&events, msg.job_id, conclusion).await;

  Ok(())
}

async fn emit_job_started(events: &mpsc::Sender<RunnerEvent>, job_id: &str, job_name: &str) {
  let _ = events
    .send(RunnerEvent::JobStarted {
      job_id: job_id.to_owned(),
      job_name: job_name.to_owned(),
    })
    .await;
}

async fn emit_job_completed(
  events: &mpsc::Sender<RunnerEvent>,
  job_id: String,
  conclusion: Conclusion,
) {
  let _ = events
    .send(RunnerEvent::JobCompleted {
      job_id,
      conclusion,
      outputs: HashMap::new(),
    })
    .await;
}

async fn start_cache_service(config: &RunnerConfig) -> Result<(CacheService, String), RunnerError> {
  let cache_dir = config.data_dir.join("cache");
  std::fs::create_dir_all(&cache_dir)?;
  let backend = LocalDiskBackend::new(cache_dir, DEFAULT_CACHE_MAX_BYTES);
  let token = uuid::Uuid::new_v4().to_string();
  let service = CacheService::start(backend, token.clone()).await?;
  Ok((service, token))
}

fn build_context(
  msg: &AgentJobRequestMessage,
  config: &RunnerConfig,
  masker: Arc<Mutex<SecretMasker>>,
) -> ExecutionContext {
  let mut ctx = ExecutionContext::with_masker(masker);

  // In Serve mode this carries the per-job cgroup so spawned steps are moved
  // into it for CPU/memory enforcement; `None` in listener/JIT mode.
  ctx.set_cgroup_path(config.cgroup_path.clone());

  // Register secrets from variables
  for (key, var) in &msg.variables {
    if var.is_secret {
      ctx.register_secret(key, &var.value);
    } else {
      ctx.set_env(key, &var.value);
    }

    // Map system.github.* variables to github context + GITHUB_* env vars
    if let Some(gh_key) = key.strip_prefix("system.github.") {
      ctx.set_github_context(gh_key, &var.value);
      let env_key = format!("GITHUB_{}", gh_key.to_uppercase().replace('.', "_"));
      ctx.set_env(&env_key, &var.value);
    }
  }

  // Extract github context from context_data (PipelineContextData dict)
  extract_github_context(&msg.context_data, &mut ctx);

  // Register mask hints
  for hint in &msg.mask {
    ctx.add_mask(&hint.value);
  }

  ctx
}

/// Write the GitHub event payload to `{data_dir}/events/{job_id}.json`.
///
/// Stored outside the workspace because `actions/checkout` wipes it.
fn write_event_json(
  data_dir: &std::path::Path,
  job_id: &str,
  ctx: &ExecutionContext,
) -> Result<String, RunnerError> {
  let events_dir = data_dir.join("events");
  std::fs::create_dir_all(&events_dir)?;
  let event_path = events_dir.join(format!("{job_id}.json"));

  let json = match ctx.github_context_value("event") {
    Some(event_value) => {
      serde_json::to_string_pretty(&event_value.to_json_value()).unwrap_or_else(|_| "{}".to_owned())
    },
    None => "{}".to_owned(),
  };

  std::fs::write(&event_path, &json)?;
  Ok(event_path.to_string_lossy().into_owned())
}

fn extract_github_context(
  context_data: &HashMap<String, shared::PipelineContextData>,
  ctx: &mut ExecutionContext,
) {
  let Some(gh) = context_data.get("github") else {
    return;
  };
  let Some(entries) = &gh.d else { return };

  for entry in entries {
    let Some(key) = &entry.key.s else {
      continue;
    };

    // String values → set both github context and GITHUB_* env var
    if let Some(value) = &entry.value.s {
      ctx.set_github_context(key, value);
      let env_key = format!("GITHUB_{}", key.to_uppercase());
      ctx.set_env(&env_key, value);
      continue;
    }

    // Non-string values (nested dicts like `event`, `repository`) →
    // store as typed ExprValue so ${{ github.event.xxx }} expressions work
    let expr_value = pipeline_data_to_expr_value(&entry.value);
    ctx.set_github_context_value(key, expr_value);
  }
}
