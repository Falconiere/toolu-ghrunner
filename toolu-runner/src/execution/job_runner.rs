use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use shared::{
  AgentJobRequestMessage, Conclusion, RunnerConfig, RunnerError, RunnerEvent, ServicesMode,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::info;

use super::cache::backend::LocalDiskBackend;
use super::cache::service::CacheService;
use super::context::ExecutionContext;
use super::expressions::context_data::pipeline_data_to_expr_value;
use super::job_hooks::{JobHookStage, run_job_hook};
use super::job_spec::{JobSpec, evaluate_job_outputs};
use super::secret_masker::SecretMasker;
use super::service_endpoints::{extract_service_urls, forward_env};
use super::steps_runner::{JobRun, run_steps};

/// Default cache size limit: 10 GB.
const DEFAULT_CACHE_MAX_BYTES: u64 = 10 * 1024 * 1024 * 1024;

/// Execute a complete job from an `AgentJobRequestMessage`.
///
/// In **forwarder** mode (default) the real GitHub service URLs + runtime token
/// are copied from the job message into step env; no local services run. In
/// **offline** mode a local cache service is started and step env points at it.
///
/// # Errors
///
/// Returns `RunnerError` if workspace creation, cache service startup, or
/// execution fails.
pub async fn run_job(
  msg: AgentJobRequestMessage,
  config: &RunnerConfig,
  cancel: CancellationToken,
  events: mpsc::Sender<RunnerEvent>,
  masker: Arc<Mutex<SecretMasker>>,
) -> Result<(), RunnerError> {
  let workspace = prepare_job_dirs(config, &msg.job_id)?;

  // Offline mode hosts a local cache service; forwarder mode leaves it `None`.
  let offline_cache = match config.services_mode {
    ServicesMode::Offline => {
      let (service, token) = start_cache_service(config).await?;
      info!(url = service.base_url(), "offline cache service started");
      Some((service, token))
    },
    ServicesMode::Forwarder => None,
  };

  let mut ctx = build_context(&msg, config, masker);
  setup_job_env(&mut ctx, &msg, config, offline_cache.as_ref())?;

  emit_job_started(&events, &msg.job_id, &msg.job_display_name).await;

  // TODO: the wire `AgentJobRequestMessage` does not yet carry typed job
  // `outputs:`/`defaults:`; populate `JobSpec` from the message once its wire
  // shape is confirmed from a captured live job. Empty spec is a no-op for
  // both on the live path.
  let spec = JobSpec::default();

  let body = JobBody {
    msg: &msg,
    config,
    cancel: &cancel,
    events: &events,
    workspace: &workspace,
    spec: &spec,
  };
  let (conclusion, outputs) = run_job_body(&body, &mut ctx).await?;

  if let Some((service, _)) = offline_cache {
    service.shutdown().await;
  }
  emit_job_completed(&events, msg.job_id, conclusion, outputs).await;

  Ok(())
}

/// Create the per-job workspace and the data dir, restricting the data dir
/// to the runner user (0o700 — it holds credentials, caches, and `_temp`
/// step payloads; a permissive umask must not leave it world-readable).
fn prepare_job_dirs(
  config: &RunnerConfig,
  job_id: &str,
) -> Result<std::path::PathBuf, RunnerError> {
  let workspace = config.workspace_root.join(job_id);
  std::fs::create_dir_all(&workspace)?;
  std::fs::create_dir_all(&config.data_dir)?;
  super::context::restrict_dir_permissions(&config.data_dir)?;
  Ok(workspace)
}

/// Borrowed inputs for the hook + step-loop + outputs phase of a job run.
struct JobBody<'a> {
  msg: &'a AgentJobRequestMessage,
  config: &'a RunnerConfig,
  cancel: &'a CancellationToken,
  events: &'a mpsc::Sender<RunnerEvent>,
  workspace: &'a std::path::Path,
  spec: &'a JobSpec,
}

/// Run the job-started hook, the step loop, job-output evaluation, and the
/// job-completed hook. Returns the job conclusion and resolved outputs.
///
/// The job-started hook is a hard gate (its failure short-circuits to a failed
/// job before any step); the job-completed hook is best-effort and never
/// overrides the conclusion. Job `outputs:` are evaluated after main + post
/// steps so step outputs are fully recorded.
async fn run_job_body(
  body: &JobBody<'_>,
  ctx: &mut ExecutionContext,
) -> Result<(Conclusion, HashMap<String, String>), RunnerError> {
  let JobBody {
    msg,
    config,
    cancel,
    events,
    workspace,
    spec,
  } = *body;

  // Job-started hook is a hard gate: its failure fails the job before any step.
  let started = run_job_hook(JobHookStage::Started, ctx, events, workspace, cancel).await?;
  if matches!(started, Some(Conclusion::Failure | Conclusion::Cancelled)) {
    return Ok((started.unwrap_or(Conclusion::Failure), HashMap::new()));
  }

  let run = JobRun {
    workspace,
    config,
    spec,
  };
  let conclusion = run_steps(&msg.steps, ctx, events, cancel.clone(), &run).await?;

  // Evaluate job `outputs:` against the final context (after main + post steps).
  let outputs = evaluate_job_outputs(spec, ctx)?;

  run_completed_hook_best_effort(ctx, events, workspace, cancel).await;

  Ok((conclusion, outputs))
}

/// Run the job-completed hook best-effort (never overrides the job
/// conclusion): a non-success hook conclusion and a spawn/read failure are
/// both logged rather than propagated (a `?` would turn a successful job into
/// an error return, breaking the documented contract).
async fn run_completed_hook_best_effort(
  ctx: &ExecutionContext,
  events: &mpsc::Sender<RunnerEvent>,
  workspace: &std::path::Path,
  cancel: &CancellationToken,
) {
  match run_job_hook(JobHookStage::Completed, ctx, events, workspace, cancel).await {
    Ok(Some(c)) if c != Conclusion::Success => {
      tracing::warn!(conclusion = ?c, "job-completed hook did not succeed; preserving job conclusion");
    },
    Ok(_) => {},
    Err(e) => {
      tracing::error!(error = ?e, "job-completed hook failed; preserving job conclusion");
    },
  }
}

/// Seed the per-job env every step inherits: the `ACTIONS_*` service vars
/// (forwarder: real URLs + token from the message; offline: the local cache
/// service + token), `GITHUB_EVENT_PATH` (written outside the workspace since
/// checkout wipes it), and a W3C trace context.
///
/// This is the central injection point — `ctx.set_env` lands on the global
/// env that `ExecutionContext::build_step_env` merges into every step.
fn setup_job_env(
  ctx: &mut ExecutionContext,
  msg: &AgentJobRequestMessage,
  config: &RunnerConfig,
  offline_cache: Option<&(CacheService, String)>,
) -> Result<(), RunnerError> {
  match offline_cache {
    // Offline: point the toolkit at the local cache service + local token.
    Some((service, token)) => {
      // Register the mask before the token is placed anywhere.
      ctx.add_mask(token);
      ctx.set_env("ACTIONS_CACHE_URL", service.base_url());
      ctx.set_env("ACTIONS_RUNTIME_TOKEN", token);
    },
    // Forwarder: copy the real GitHub service URLs + runtime token from the
    // message. A `None` URL is omitted (WARN); never an empty var.
    None => {
      for (key, value) in forward_env(&extract_service_urls(msg)) {
        // Any `*_TOKEN` var is a credential (runtime token, id-token request
        // token, and any future one) — register it with the masker before it
        // is placed in the env so it never reaches the diag log or journal.
        if key.ends_with("_TOKEN") {
          ctx.add_mask(&value);
        }
        ctx.set_env(&key, &value);
      }
    },
  }

  // Write event.json outside workspace (checkout wipes workspace contents).
  let event_path = write_event_json(&config.data_dir, &msg.job_id, ctx)?;
  ctx.set_env("GITHUB_EVENT_PATH", &event_path);

  // Inject W3C trace context for distributed trace correlation.
  let trace_id = uuid::Uuid::new_v4().to_string().replace('-', "");
  let span_id = &trace_id[..16];
  ctx.set_env("TRACEPARENT", &format!("00-{trace_id}-{span_id}-01"));
  ctx.set_env("TRACESTATE", "toolu=true");
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
  outputs: HashMap<String, String>,
) {
  let _ = events
    .send(RunnerEvent::JobCompleted {
      job_id,
      conclusion,
      outputs,
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

/// Build the per-job `ExecutionContext` from the job message + runner config.
///
/// Populates `runner.*` (host/config), `github.*` and `vars.*` (from the
/// message `contextData`), `secrets.*` (from `variables` where `is_secret`),
/// and the secret masker. `pub` so hermetic tests can drive the real
/// context-assembly path. Best-effort on `runner.*` dir creation: a failure
/// is logged and the run continues without the env mirror.
pub fn build_context(
  msg: &AgentJobRequestMessage,
  config: &RunnerConfig,
  masker: Arc<Mutex<SecretMasker>>,
) -> ExecutionContext {
  let mut ctx = ExecutionContext::with_masker(masker);

  // In Serve mode this carries the per-job cgroup so spawned steps are moved
  // into it for CPU/memory enforcement; `None` in listener/JIT mode.
  ctx.set_cgroup_path(config.cgroup_path.clone());

  // Variables: secrets (is_secret) → secrets.* + masker; the auto github
  // token (`system.github.token`) is excluded from secrets.* and routed to
  // github.token instead (matches actions/runner). Non-secret system vars
  // become env only — repo config variables arrive via contextData["vars"].
  for (key, var) in &msg.variables {
    if var.is_secret {
      ctx.register_secret_masked(&var.value);
      if let Some(gh_key) = key.strip_prefix("system.github.") {
        ctx.set_github_context(gh_key, &var.value);
        ctx.set_env(&github_env_key(gh_key), &var.value);
      } else {
        ctx.register_secret(key, &var.value);
      }
    } else {
      ctx.set_env(key, &var.value);
    }
  }

  // github.* + vars.* from contextData dicts.
  extract_github_context(&msg.context_data, &mut ctx);
  extract_vars_context(&msg.context_data, &mut ctx);

  // runner.* from host + config (name falls back to the message runner dict).
  let runner_name = runner_name(msg);
  if let Err(e) = ctx.set_runner_context(&runner_name, &config.data_dir) {
    tracing::warn!(error = %e, "failed to materialize runner.temp/tool_cache dirs");
  }

  // Mask hints from the job message.
  for hint in &msg.mask {
    ctx.add_mask(&hint.value);
  }

  ctx
}

/// Runner name: the message's `runner.name` context value, else the hostname.
fn runner_name(msg: &AgentJobRequestMessage) -> String {
  msg
    .context_data
    .get("runner")
    .and_then(|runner| runner.d.as_ref())
    .into_iter()
    .flatten()
    .find(|entry| entry.key.s.as_deref() == Some("name"))
    .and_then(|entry| entry.value.s.clone())
    .unwrap_or_else(fallback_hostname)
}

/// Hostname for the runner name, or a stable fallback if unavailable.
fn fallback_hostname() -> String {
  hostname::get()
    .ok()
    .and_then(|h| h.into_string().ok())
    .unwrap_or_else(|| "toolu-runner".to_owned())
}

/// Populate `vars.*` from `contextData["vars"]` (repo/org/env config variables).
fn extract_vars_context(
  context_data: &HashMap<String, shared::PipelineContextData>,
  ctx: &mut ExecutionContext,
) {
  let Some(vars) = context_data.get("vars") else {
    return;
  };
  let Some(entries) = &vars.d else { return };
  for entry in entries {
    let (Some(key), Some(value)) = (&entry.key.s, &entry.value.s) else {
      continue;
    };
    ctx.set_var(key, value);
  }
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

/// Canonical `github.<name>` → `GITHUB_<NAME>` env-var key transform.
///
/// Uppercases and replaces `.` with `_` (mirroring the `INPUT_` transform), so
/// a dotted context key like `event.name` becomes a valid `GITHUB_EVENT_NAME`
/// rather than an invalid `GITHUB_EVENT.NAME`. Used by both the secret
/// `system.github.*` path and the `contextData["github"]` path so they agree.
fn github_env_key(name: &str) -> String {
  format!("GITHUB_{}", name.to_uppercase().replace('.', "_"))
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
      ctx.set_env(&github_env_key(key), value);
      continue;
    }

    // Non-string values (nested dicts like `event`, `repository`) →
    // store as typed ExprValue so ${{ github.event.xxx }} expressions work
    let expr_value = pipeline_data_to_expr_value(&entry.value);
    ctx.set_github_context_value(key, expr_value);
  }
}
