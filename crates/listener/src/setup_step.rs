//! Initial setup identity, metadata and best-effort in-progress reporting.

use shared::{AgentJobRequestMessage, SecretMasker};
use std::collections::BTreeMap;
use wire::reporting::Status;
use wire::reporting::results_service::{
  StepUpdateEntry, WorkflowStepsUpdateRequest, update_workflow_steps,
};

/// Open setup before the live-log handshake; logs stream through the forwarder.
pub(super) async fn report_setup_step(
  token: &str,
  plan_id: &str,
  job_msg: &AgentJobRequestMessage,
  client: &reqwest::Client,
) -> (String, Vec<String>) {
  let external_id = uuid::Uuid::new_v4().to_string();
  let now = chrono::Utc::now().to_rfc3339();
  if let Some(results_url) = job_msg.variables.get("system.github.results_endpoint") {
    let (run_backend_id, job_backend_id) = super::helpers::resolve_backend_ids(job_msg, plan_id);
    let request = WorkflowStepsUpdateRequest {
      steps: vec![StepUpdateEntry {
        external_id: external_id.clone(),
        number: 1,
        name: "Set up job".to_owned(),
        status: Status::InProgress,
        conclusion: None,
        started_at: Some(now.clone()),
        completed_at: None,
      }],
      change_order: 0,
      workflow_run_backend_id: run_backend_id,
      workflow_job_run_backend_id: job_backend_id,
    };
    if let Err(error) = update_workflow_steps(
      client,
      results_url.value.trim_end_matches('/'),
      token,
      &request,
    )
    .await
    {
      tracing::warn!(%error, "setup step report failed");
    }
  }
  (external_id, metadata_lines(job_msg))
}

/// Read only diagnostic metadata; malformed permissions never echo their input.
fn metadata_lines(msg: &AgentJobRequestMessage) -> Vec<String> {
  let machine = hostname::get()
    .ok()
    .and_then(|name| name.into_string().ok());
  let mut lines = vec![
    format!("Toolu runner version: '{}'", env!("CARGO_PKG_VERSION")),
    format!(
      "Runner compatibility version: '{}'",
      protocol::runner_version::COMPATIBILITY_VERSION
    ),
    format!("Operating system: '{}'", shared::platform::runner_os()),
    format!("Architecture: '{}'", shared::platform::runner_arch()),
    format!("Machine name: '{}'", available(machine.as_deref())),
    format!(
      "Runner group name: '{}'",
      available(variable(msg, "system.runnerGroupName"))
    ),
  ];
  if let Some(raw) = variable(msg, "system.github.token.permissions") {
    match serde_json::from_str::<BTreeMap<String, String>>(raw) {
      Ok(permissions) => {
        lines.push("##[group]GITHUB_TOKEN Permissions".to_owned());
        lines.extend(
          permissions
            .into_iter()
            .map(|(key, value)| format!("{key}: {value}")),
        );
        lines.push("##[endgroup]".to_owned());
      },
      Err(_) => lines.push("Unable to parse GITHUB_TOKEN permissions metadata.".to_owned()),
    }
  }
  if let Some(source) = msg
    .context_data
    .get("github")
    .and_then(|context| context.d.as_ref())
    .and_then(|entries| {
      entries
        .iter()
        .find(|entry| entry.key.s.as_deref() == Some("secret_source"))
    })
    .and_then(|entry| entry.value.s.as_deref())
    .filter(|value| !value.is_empty())
  {
    lines.push(format!("Secret source: {source}"));
  }
  lines
}

fn variable<'a>(msg: &'a AgentJobRequestMessage, name: &str) -> Option<&'a str> {
  msg
    .variables
    .iter()
    .find(|(key, _)| key.eq_ignore_ascii_case(name))
    .map(|(_, value)| value.value.as_str())
    .filter(|value| !value.is_empty())
}

/// Explicit absence rather than an invented runner identity.
pub(super) fn available(value: Option<&str>) -> &str {
  value
    .filter(|value| !value.is_empty())
    .unwrap_or("unavailable")
}

/// Register acquired secrets before setup reaches any log sink.
pub(super) fn register_masks(msg: &AgentJobRequestMessage, masker: &mut SecretMasker) {
  masker.add_secrets(
    msg
      .variables
      .iter()
      .filter(|(key, value)| value.is_secret || key.eq_ignore_ascii_case("system.github.token"))
      .map(|(_, value)| value.value.as_str())
      .chain(
        msg
          .resources
          .endpoints
          .iter()
          .filter_map(|endpoint| endpoint.authorization.as_ref())
          .flat_map(|authorization| authorization.parameters.values().map(String::as_str)),
      ),
  );
  for hint in &msg.mask {
    masker.add_mask(&hint.value);
  }
}

#[cfg(test)]
#[path = "tests/setup_step.rs"]
mod tests;
