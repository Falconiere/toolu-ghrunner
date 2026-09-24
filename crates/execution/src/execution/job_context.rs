//! Message-derived job context and host event-file materialization.

use super::context::ExecutionContext;
use super::service_endpoints::extract_service_urls;
use expressions::context_data::pipeline_data_to_expr_value;
use shared::{AgentJobRequestMessage, RunnerConfig, RunnerError, SecretMasker};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Build the per-job `ExecutionContext` from the job message + runner config.
///
/// Populates `runner.*` (host/config), `github.*` and `vars.*` (from the
/// message `contextData`), `secrets.*` (from `variables` where `is_secret`),
/// and the secret masker. `pub` so hermetic tests can drive the real
/// context-assembly path. Best-effort on `runner.*` dir creation: a failure
/// is logged and the run continues without the env mirror.
pub(super) fn build_context(
  msg: &AgentJobRequestMessage,
  config: &RunnerConfig,
  masker: Arc<Mutex<SecretMasker>>,
) -> ExecutionContext {
  let mut ctx = ExecutionContext::with_masker(masker);
  ctx.import_contexts(&msg.context_data);

  // In Serve mode this carries the per-job cgroup so spawned steps are moved
  // into it for CPU/memory enforcement; `None` in listener/JIT mode.
  ctx.set_cgroup_path(config.cgroup_path.clone());

  // Variables (secrets.* / env) plus the runtime service token → masker.
  register_message_variables(&mut ctx, msg);

  // github.* + vars.* from contextData dicts.
  extract_github_context(&msg.context_data, &mut ctx);
  extract_vars_context(&msg.context_data, &mut ctx);

  // runner.* from host + config (name falls back to the message runner dict).
  let runner_name = runner_name(msg);
  if let Err(e) = ctx.set_runner_context(&runner_name, &config.data_dir) {
    tracing::warn!(error = %e, "failed to materialize runner.temp/tool_cache dirs");
  }

  // Mask hints from the job message, one batch rebuild instead of one per hint.
  ctx.add_masks(msg.mask.iter().map(|hint| hint.value.as_str()));

  ctx
}

/// Register the job message's `variables` and the Actions runtime token into
/// `ctx`.
///
/// Secrets (`is_secret`) go to `secrets.*` + the masker; the auto
/// `system.github.token` is routed to `github.token` instead of `secrets.*`
/// (matches actions/runner); non-secret vars become env only. The runtime
/// service token is masked like a secret so it never reaches a log unredacted.
/// Every mask registration is collected and applied through the batch APIs
/// (`add_masks` / `register_secrets`) instead of rebuilding the automaton
/// once per variable.
fn register_message_variables(ctx: &mut ExecutionContext, msg: &AgentJobRequestMessage) {
  let mut mask_only: Vec<String> = Vec::new();
  let mut secret_pairs: Vec<(String, String)> = Vec::new();

  for (key, var) in &msg.variables {
    if var.is_secret {
      mask_only.push(var.value.clone());
      if let Some(gh_key) = key.strip_prefix("system.github.") {
        ctx.set_github_context(gh_key, &var.value);
        ctx.set_env(&github_env_key(gh_key), &var.value);
      } else {
        secret_pairs.push((key.clone(), var.value.clone()));
      }
    } else {
      ctx.set_env(key, &var.value);
    }
  }

  // The Actions runtime token (from the job message's SystemVssConnection)
  // authenticates the cache / artifact / OIDC services and is forwarded
  // upstream and injected as `ACTIONS_RUNTIME_TOKEN`. Register it with the
  // masker — like actions/runner, which treats it as a secret — so it can never
  // reach `_diag/runner.log` or a step log unredacted.
  let runtime_token = extract_service_urls(msg).runtime_token;
  if !runtime_token.is_empty() {
    mask_only.push(runtime_token);
  }

  ctx.add_masks(mask_only);
  ctx.register_secrets(secret_pairs);
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
pub(super) fn write_event_json(
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
