use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use shared::{
  AgentJobRequestMessage, Conclusion, RunnerConfig, RunnerError, RunnerEvent, ServicesMode,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::info;

use super::cache::accelerated::{AcceleratedInputs, accelerated_app};
use super::cache::blob::{BlobRegistry, sweep_staging};
use super::cache::cas::{CacheGc, CacheIndex, CasStore, LeaseSet};
use super::cache::scope::{CacheScopes, scopes_for_job};
use super::cache::server::CacheServer;
use super::cache::trust::{TrustLevel, classify_trust};
use super::cache::v1::{V1Inputs, V1State, v1_router};
use super::context::ExecutionContext;
use super::expressions::context_data::pipeline_data_to_expr_value;
use super::job_hooks::{JobHookStage, run_job_hook};
use super::job_spec::{JobSpec, evaluate_job_outputs};
use super::secret_masker::SecretMasker;
use super::service_endpoints::{ServiceUrls, extract_service_urls, forward_env};
use super::shadow::ShadowObserver;
use super::steps_runner::{JobRun, run_steps};

/// The local services a job's configured mode brought up, threaded from the
/// startup match through `setup_job_env` and shut down at job end.
///
/// Accelerated carries the real [`ServiceUrls`] alongside the server so
/// `setup_job_env` can forward the non-cache vars (real GitHub URLs + token)
/// before overriding the cache vars at the local server.
enum LocalServices {
  /// Forwarder: no local services; step env forwards the message's real URLs.
  None,
  /// Offline: a local cache server + its fresh bearer token + teardown handles.
  Offline(CacheServer, String, CacheMaintenance),
  /// Accelerated: a local cache server + the real service URLs to forward +
  /// teardown handles. The runtime token needs no separate field — it rides in
  /// `ServiceUrls` and `setup_job_env` forwards it verbatim.
  Accelerated(CacheServer, ServiceUrls, CacheMaintenance),
}

/// The shared CAS handles a job's local cache server was built over, retained so
/// [`run_cache_maintenance`] can sweep staging and GC the store at teardown.
///
/// Every handle is a cheap clone onto the same on-disk root / in-memory lease
/// map the running server holds, so GC honours any in-flight read lease.
struct CacheMaintenance {
  store: CasStore,
  index: CacheIndex,
  leases: LeaseSet,
  staging_root: std::path::PathBuf,
}

/// Age past which an abandoned `cas/staging` upload is swept at job teardown.
///
/// Comfortably exceeds a single upload window; single-job concurrency (the
/// `.lock`) means nothing else is mid-upload when teardown runs.
const STAGING_SWEEP_TTL: Duration = Duration::from_secs(3600);

/// Execute a complete job from an `AgentJobRequestMessage`.
///
/// The configured [`ServicesMode`] decides step env: forwarder copies the
/// message's real URLs + token; offline points cache vars at a local server;
/// accelerated serves both cache protocols local and proxies the rest.
///
/// # Errors
///
/// Returns `RunnerError` if workspace creation, cache startup, or execution
/// fails.
pub async fn run_job(
  msg: AgentJobRequestMessage,
  config: &RunnerConfig,
  cancel: CancellationToken,
  events: mpsc::Sender<RunnerEvent>,
  masker: Arc<Mutex<SecretMasker>>,
) -> Result<(), RunnerError> {
  let workspace = prepare_job_dirs(config, &msg.job_id)?;
  gc_stale_workspaces(config, &msg.job_id);

  // Build the context and set the workspace before starting local services:
  // accelerated mode needs the job's github context to compute cache scopes.
  let mut ctx = build_context(&msg, config, masker);
  ctx.set_workspace(Some(workspace.clone()));

  let local = start_local_services(config, &msg, &ctx).await?;
  setup_job_env(&mut ctx, &msg, config, &local)?;

  // Shadow-mode step observer (approach C): records would-hit / false-hit per
  // `run:` step and NEVER serves a cached result. Inert unless `shadow_enabled`.
  let shadow = ShadowObserver::new(
    config.shadow_enabled,
    &config.data_dir,
    &msg.job_id,
    Arc::clone(ctx.masker()),
  );

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
    shadow: &shadow,
  };
  let (conclusion, outputs) = run_job_body(&body, &mut ctx).await?;

  shutdown_local_services(local, config).await;
  emit_job_completed(&events, msg.job_id, conclusion, outputs).await;

  Ok(())
}

/// Bring up whatever local cache services the configured mode needs, before
/// step env is seeded.
///
/// The match is exhaustive over every [`ServicesMode`] arm — `wildcard_enum_match_arm`
/// forbids a `_` catch-all, so a new mode must be handled explicitly here.
///
/// # Errors
///
/// Returns `RunnerError::Cache` if a local server cannot bind its port.
async fn start_local_services(
  config: &RunnerConfig,
  msg: &AgentJobRequestMessage,
  ctx: &ExecutionContext,
) -> Result<LocalServices, RunnerError> {
  match config.services_mode {
    ServicesMode::Offline => {
      let (service, token, maintenance) = start_cache_service(config).await?;
      info!(url = service.base_url(), "offline cache service started");
      Ok(LocalServices::Offline(service, token, maintenance))
    },
    ServicesMode::Accelerated => {
      // The token is discarded: it already rides in `urls` (forwarded verbatim)
      // and backs the app's bearer, so `LocalServices` need not carry it again.
      let (service, urls, _token, maintenance) =
        start_accelerated_service(config, msg, ctx).await?;
      info!(
        url = service.base_url(),
        "accelerated cache service started"
      );
      Ok(LocalServices::Accelerated(service, urls, maintenance))
    },
    ServicesMode::Forwarder => Ok(LocalServices::None),
  }
}

/// Shut down any local cache server the job brought up (offline / accelerated),
/// then run best-effort cache maintenance over its retained CAS handles.
async fn shutdown_local_services(local: LocalServices, config: &RunnerConfig) {
  match local {
    LocalServices::Offline(service, _, maintenance)
    | LocalServices::Accelerated(service, _, maintenance) => {
      service.shutdown().await;
      run_cache_maintenance(config, &maintenance).await;
    },
    LocalServices::None => {},
  }
}

/// Best-effort cache teardown: sweep abandoned staging uploads, then run one GC
/// pass (TTL expiry + `max_bytes` eviction + unreferenced-blob sweep).
///
/// Neither step may fail the job: errors are logged and swallowed. Runs after
/// the server has stopped, so no handler holds a read lease GC must respect.
async fn run_cache_maintenance(config: &RunnerConfig, maintenance: &CacheMaintenance) {
  sweep_staging_best_effort(&maintenance.staging_root);
  gc_best_effort(config, maintenance).await;
}

/// Sweep abandoned `cas/staging` uploads best-effort; log and swallow errors.
fn sweep_staging_best_effort(staging_root: &std::path::Path) {
  match sweep_staging(staging_root, STAGING_SWEEP_TTL) {
    Ok(0) => {},
    Ok(n) => info!(removed = n, "cache staging sweep removed abandoned uploads"),
    Err(e) => tracing::warn!(error = %e, "cache staging sweep failed; continuing"),
  }
}

/// Run one GC pass over the retained CAS handles best-effort; never fails the job.
async fn gc_best_effort(config: &RunnerConfig, maintenance: &CacheMaintenance) {
  let gc = CacheGc::new(config.cache.entry_ttl_days, config.cache.max_bytes);
  match gc
    .run(&maintenance.store, &maintenance.index, &maintenance.leases)
    .await
  {
    Ok(report) => info!(?report, "cache GC pass complete"),
    Err(e) => tracing::warn!(error = %e, "cache GC failed; continuing"),
  }
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

/// Ceiling on `workspace_gc_hours` (100 years) so the seconds conversion cannot
/// saturate. A configured value at or above the cap means "never prune".
const MAX_GC_HOURS: u64 = 100 * 365 * 24;

/// Prune stale per-job workspaces best-effort, sparing the running job `keep`.
///
/// GC failure must never fail the job, so an error is logged and the run
/// continues; the count of pruned directories is logged at INFO.
fn gc_stale_workspaces(config: &RunnerConfig, keep: &str) {
  let hours = config.workspace_gc_hours.min(MAX_GC_HOURS);
  let max_age = Duration::from_secs(hours * 3600);
  match super::workspace_gc::gc_workspaces(&config.workspace_root, max_age, keep) {
    Ok(0) => {},
    Ok(n) => info!(removed = n, "workspace GC pruned stale job workspaces"),
    Err(e) => tracing::warn!(error = %e, "workspace GC failed; continuing"),
  }
}

/// Borrowed inputs for the hook + step-loop + outputs phase of a job run.
struct JobBody<'a> {
  msg: &'a AgentJobRequestMessage,
  config: &'a RunnerConfig,
  cancel: &'a CancellationToken,
  events: &'a mpsc::Sender<RunnerEvent>,
  workspace: &'a std::path::Path,
  spec: &'a JobSpec,
  /// Shadow-mode step observer threaded into the step loop (records, never
  /// serves).
  shadow: &'a ShadowObserver,
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
    shadow,
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
    shadow: Some(shadow),
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

/// Seed the per-job env every step inherits: the `ACTIONS_*` service vars (by
/// mode), `GITHUB_EVENT_PATH` (written outside the workspace since checkout
/// wipes it), and a W3C trace context.
///
/// This is the central injection point — `ctx.set_env` lands on the global
/// env that `ExecutionContext::build_step_env` merges into every step.
fn setup_job_env(
  ctx: &mut ExecutionContext,
  msg: &AgentJobRequestMessage,
  config: &RunnerConfig,
  local: &LocalServices,
) -> Result<(), RunnerError> {
  match local {
    LocalServices::Offline(service, token, _) => setup_offline_env(ctx, service, token),
    LocalServices::Accelerated(service, urls, _) => setup_accelerated_env(ctx, service, urls),
    LocalServices::None => apply_forwarded_env(ctx, &extract_service_urls(msg)),
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

/// Offline env: point the toolkit at the local cache service + local token.
fn setup_offline_env(ctx: &mut ExecutionContext, service: &CacheServer, token: &str) {
  // Register the mask before the token is placed anywhere.
  ctx.add_mask(token);
  ctx.set_env("ACTIONS_CACHE_URL", service.base_url());
  ctx.set_env("ACTIONS_RUNTIME_TOKEN", token);
}

/// Accelerated env: forward the real non-cache vars (URLs + token), then
/// override exactly the three cache vars at the local server.
///
/// `ACTIONS_RUNTIME_TOKEN` is deliberately left as the real GitHub token — the
/// local server's bearer equals it and the reverse proxy forwards it upstream
/// for artifacts. Only cache traffic is redirected local.
fn setup_accelerated_env(ctx: &mut ExecutionContext, service: &CacheServer, urls: &ServiceUrls) {
  apply_forwarded_env(ctx, urls);
  let base = service.base_url().to_owned();
  ctx.set_env("ACTIONS_RESULTS_URL", &base);
  ctx.set_env("ACTIONS_CACHE_URL", &base);
  ctx.set_env("ACTIONS_CACHE_SERVICE_V2", "true");
}

/// Forwarder env: copy the real GitHub service URLs + runtime token from the
/// message into step env, masking every `*_TOKEN` credential first.
///
/// A `None` URL is omitted (WARN); never an empty var. Any `*_TOKEN` var (the
/// runtime token, the id-token request token, and any future one) is registered
/// with the masker before it reaches the env, so it never leaks to the diag log
/// or journal.
fn apply_forwarded_env(ctx: &mut ExecutionContext, urls: &ServiceUrls) {
  for (key, value) in forward_env(urls) {
    if key.ends_with("_TOKEN") {
      ctx.add_mask(&value);
    }
    ctx.set_env(&key, &value);
  }
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

/// Start the offline v1 REST cache server over the content-addressed store.
///
/// Offline mode is hermetic/airgapped, so a single permissive `offline` scope
/// (no branch isolation) backs both the write scope and the read ladder. The
/// server binds `config.service_bind:0` (ephemeral port) and reports a loopback
/// `base_url()` for step env. Returns the server plus a fresh bearer token.
async fn start_cache_service(
  config: &RunnerConfig,
) -> Result<(CacheServer, String, CacheMaintenance), RunnerError> {
  let maintenance = build_cas_handles(config)?;
  let token = uuid::Uuid::new_v4().to_string();
  // Offline is hermetic: one permissive `offline` scope, always writable, so
  // trust is `Trusted` with no protected set.
  let state = V1State::new(V1Inputs {
    store: maintenance.store.clone(),
    index: maintenance.index.clone(),
    leases: maintenance.leases.clone(),
    scopes: CacheScopes {
      write: "offline".to_owned(),
      read_ladder: vec!["offline".to_owned()],
    },
    trust: TrustLevel::Trusted,
    protected: Vec::new(),
    bearer: token.clone(),
    staging_root: maintenance.staging_root.clone(),
  });
  let bind = format!("{}:0", config.service_bind);
  let server = CacheServer::start(v1_router(state), &bind).await?;
  Ok((server, token, maintenance))
}

/// Build the per-job CAS handles + their retained teardown maintenance set.
///
/// Both local modes share one on-disk cache root (`data_dir/cache`); the
/// returned [`CacheMaintenance`] is the canonical owner, and each caller clones
/// its handles into the server state, so teardown GC sees the same store, index,
/// and live-lease map the server used.
///
/// # Errors
///
/// Returns `RunnerError::Io` if the staging directory cannot be created.
fn build_cas_handles(config: &RunnerConfig) -> Result<CacheMaintenance, RunnerError> {
  let cache_dir = config.data_dir.join("cache");
  let staging_root = cache_dir.join("staging");
  std::fs::create_dir_all(&staging_root)?;
  let store = CasStore::new(
    cache_dir.clone(),
    config.cache.chunk_avg_bytes,
    config.cache.max_bytes,
  );
  let index = CacheIndex::new(cache_dir);
  Ok(CacheMaintenance {
    store,
    index,
    leases: LeaseSet::new(),
    staging_root,
  })
}

/// Start the accelerated cache app over a per-job content-addressed store.
///
/// Computes cache scopes + write trust from the github context, reads the real
/// results URL + runtime token from the message, and binds the v2/v1/blob app +
/// reverse proxy. Returns the server, the real [`ServiceUrls`], the token, and
/// the teardown [`CacheMaintenance`] handles.
///
/// # Errors
///
/// `RunnerError::Config` if the runtime token is empty (an empty bearer would
/// open the cache); otherwise `RunnerError` if the staging dir or bind fails.
async fn start_accelerated_service(
  config: &RunnerConfig,
  msg: &AgentJobRequestMessage,
  ctx: &ExecutionContext,
) -> Result<(CacheServer, ServiceUrls, String, CacheMaintenance), RunnerError> {
  let scopes = scopes_for_job(ctx, &config.cache.protected_branches);
  let event = ctx.github_context("event_name").unwrap_or_default();
  let ref_name = ctx.github_context("ref_name").unwrap_or_default();
  let trust = classify_trust(event, ref_name, &config.cache.protected_branches);

  let urls = extract_service_urls(msg);
  let upstream_results_url = urls.results_url.clone().unwrap_or_default();
  let bearer = urls.runtime_token.clone();
  // An empty runtime token would make the local bearer accept `Bearer ` and the
  // proxy forward no credential — fail loudly rather than serve an open cache.
  if bearer.is_empty() {
    return Err(RunnerError::Config(
      "accelerated mode requires a runtime token".to_owned(),
    ));
  }

  let maintenance = build_cas_handles(config)?;
  let app = accelerated_app(AcceleratedInputs {
    store: maintenance.store.clone(),
    index: maintenance.index.clone(),
    registry: BlobRegistry::new(),
    leases: maintenance.leases.clone(),
    scopes,
    trust,
    protected: config.cache.protected_branches.clone(),
    bearer: bearer.clone(),
    staging_root: maintenance.staging_root.clone(),
    upstream_results_url,
    client: reqwest::Client::new(),
  });

  let bind = format!("{}:0", config.service_bind);
  let server = CacheServer::start(app, &bind).await?;
  Ok((server, urls, bearer, maintenance))
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
