//! Shared helpers for the listener lifecycle.

use std::collections::HashMap;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::SessionCtx;
use crate::net::delete_session;
use crate::reporting::ReportConclusion;
use crate::reporting::run_service::{RenewJobRequest, renew_job};
use shared::{AgentJobRequestMessage, Conclusion, ListenerEvent, RunnerEvent};

/// Per-step metadata captured on `StepStarted` so later `StepCompleted` events
/// can emit a full Step record (actions/runner C# always sends both
/// `started_at` and `completed_at` on every update — null clobbers the value).
#[derive(Debug, Clone)]
pub(super) struct StepMeta {
  pub name: String,
  pub number: u32,
  pub started_at: String,
}

pub(super) type StepMetaMap = HashMap<String, StepMeta>;

/// Bundled context for Results Service calls — groups the constant-per-job
/// parameters so `report_step_to_results` / `StepLogStreamer` stay within the
/// 6-arg clippy cap.
pub struct ResultsCtx<'a> {
  pub client: &'a reqwest::Client,
  pub results_url: &'a str,
  pub token: &'a str,
  pub run_backend_id: &'a str,
  pub job_backend_id: &'a str,
}

/// Resolve Results Service backend IDs from job message variables (matches C# runner).
/// Falls back to `plan_id`/`job_id` for older GHES without these variables.
pub(super) fn resolve_backend_ids(
  job_msg: &AgentJobRequestMessage,
  plan_id: &str,
) -> (String, String) {
  let run_id = job_msg
    .variables
    .get("system.github.run_backend_id")
    .map(|v| v.value.clone())
    .unwrap_or_else(|| plan_id.to_owned());
  let job_id = job_msg
    .variables
    .get("system.github.job_backend_id")
    .map(|v| v.value.clone())
    .unwrap_or_else(|| job_msg.job_id.clone());
  (run_id, job_id)
}

/// Owned parameters for the lock renewal background task.
pub(super) struct RenewalParams {
  pub(super) client: reqwest::Client,
  pub(super) token: String,
  pub(super) run_service_url: String,
  pub(super) plan_id: String,
  pub(super) job_id: String,
  pub(super) tx: mpsc::Sender<ListenerEvent>,
}

pub(super) fn spawn_renewal(
  params: RenewalParams,
  cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
  tokio::spawn(async move {
    let req = RenewJobRequest {
      plan_id: params.plan_id,
      job_id: params.job_id,
    };
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
    interval.tick().await; // skip first immediate tick
    loop {
      tokio::select! {
        biased;
        () = cancel.cancelled() => break,
        _ = interval.tick() => {
          match renew_job(&params.client, &params.run_service_url, &params.token, &req).await {
            Ok(resp) => {
              let _ = params.tx.send(ListenerEvent::LockRenewed { locked_until: resp.locked_until }).await;
            },
            Err(e) => tracing::warn!(error = %e, "lock renewal failed"),
          }
        },
      }
    }
  })
}

pub(super) fn map_conclusion(c: Conclusion) -> ReportConclusion {
  match c {
    Conclusion::Success => ReportConclusion::Success,
    Conclusion::Failure => ReportConclusion::Failure,
    Conclusion::Cancelled => ReportConclusion::Cancelled,
    Conclusion::Skipped => ReportConclusion::Skipped,
  }
}

/// Look up the `SystemVssConnection` endpoint's `AccessToken` from a job
/// request message.
///
/// Used by both the live-log WebSocket connection (which authenticates the
/// streaming channel) and the run-service token exchange (which authenticates
/// the Twirp RPCs). Both sites must agree on which endpoint supplies the
/// bearer token; this helper is the single chokepoint that picks the
/// `SystemVssConnection` endpoint case-insensitively and reads the
/// `AccessToken` parameter case-insensitively.
///
/// Returns `None` if no such endpoint or parameter is present.
pub(super) fn system_vss_access_token(job_msg: &AgentJobRequestMessage) -> Option<String> {
  job_msg
    .resources
    .endpoints
    .iter()
    .find(|e| e.name.eq_ignore_ascii_case("SystemVssConnection"))
    .and_then(|e| e.authorization.as_ref())
    .and_then(|a| {
      a.parameters
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("AccessToken"))
        .map(|(_, v)| v.clone())
    })
}

pub(super) async fn cleanup_session(ctx: &SessionCtx) {
  tokio::time::sleep(std::time::Duration::from_secs(5)).await;
  let _ = delete_session(&ctx.client, &ctx.broker_url, &ctx.token, &ctx.session_id).await;
}

/// Report a step event to GitHub's Results Service. Errors are logged, not propagated.
pub(super) async fn report_step_to_results(
  rctx: &ResultsCtx<'_>,
  event: &RunnerEvent,
  change_order: &mut u64,
  step_meta: &mut StepMetaMap,
) {
  use crate::reporting::results_service::{WorkflowStepsUpdateRequest, update_workflow_steps};

  let Some(entry) = build_step_entry(event, step_meta) else {
    return;
  };

  let order = *change_order;
  *change_order += 1;

  let request = WorkflowStepsUpdateRequest {
    steps: vec![entry],
    change_order: order,
    workflow_run_backend_id: rctx.run_backend_id.to_owned(),
    workflow_job_run_backend_id: rctx.job_backend_id.to_owned(),
  };

  let prefix_len = std::cmp::min(10, rctx.token.len());
  let token_prefix = rctx.token.get(..prefix_len).map_or("", |s| s);

  if let Err(e) = update_workflow_steps(rctx.client, rctx.results_url, rctx.token, &request).await {
    tracing::warn!(
      error = %e,
      token_prefix,
      url = rctx.results_url,
      run_backend_id = rctx.run_backend_id,
      job_backend_id = rctx.job_backend_id,
      "results service step update failed"
    );
  }
}

/// Build the `StepUpdateEntry` for an event, updating `step_meta` as needed.
/// Returns `None` for events that don't map to a step update.
fn build_step_entry(
  event: &RunnerEvent,
  step_meta: &mut StepMetaMap,
) -> Option<crate::reporting::results_service::StepUpdateEntry> {
  use crate::reporting::Status;
  use crate::reporting::results_service::StepUpdateEntry;

  match event {
    RunnerEvent::StepStarted {
      step_id,
      step_name,
      step_number,
    } => {
      let started_at = chrono::Utc::now().to_rfc3339();
      step_meta.insert(
        step_id.clone(),
        StepMeta {
          name: step_name.clone(),
          number: *step_number,
          started_at: started_at.clone(),
        },
      );
      Some(StepUpdateEntry {
        external_id: step_id.clone(),
        number: *step_number,
        name: step_name.clone(),
        status: Status::InProgress,
        conclusion: None,
        started_at: Some(started_at),
        completed_at: None,
      })
    },
    RunnerEvent::StepCompleted {
      step_id,
      conclusion,
      ..
    } => {
      let meta = step_meta.get(step_id).cloned().unwrap_or(StepMeta {
        name: String::new(),
        number: 0,
        started_at: chrono::Utc::now().to_rfc3339(),
      });
      Some(StepUpdateEntry {
        external_id: step_id.clone(),
        number: meta.number,
        name: meta.name,
        status: Status::Completed,
        conclusion: Some(map_conclusion(*conclusion)),
        started_at: Some(meta.started_at),
        completed_at: Some(chrono::Utc::now().to_rfc3339()),
      })
    },
    RunnerEvent::JobStarted { .. }
    | RunnerEvent::JobCompleted { .. }
    | RunnerEvent::StepSkipped { .. }
    | RunnerEvent::Log { .. }
    | RunnerEvent::LogGroup { .. }
    | RunnerEvent::Annotation { .. } => None,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  /// Build an `AgentJobRequestMessage` with a single endpoint carrying one
  /// authorization parameter. Uses JSON as the construction surface so the
  /// test only depends on the public wire shape, not on internal struct
  /// fields.
  fn job_msg_with_endpoint(name: &str, key: &str, value: &str) -> AgentJobRequestMessage {
    let json = format!(
      r#"{{
        "messageType": "JobRequest",
        "plan": {{ "planId": "p1" }},
        "jobId": "1",
        "jobDisplayName": "test",
        "jobName": "test",
        "resources": {{
          "endpoints": [{{
            "name": {name:?},
            "authorization": {{
              "scheme": "OAuth",
              "parameters": {{ {key:?}: {value:?} }}
            }}
          }}]
        }}
      }}"#,
    );
    serde_json::from_str(&json).expect("valid job message")
  }

  #[test]
  fn lookup_finds_canonical_casing() {
    let msg = job_msg_with_endpoint("SystemVssConnection", "AccessToken", "tok-1");
    assert_eq!(system_vss_access_token(&msg).as_deref(), Some("tok-1"));
  }

  #[test]
  fn lookup_finds_lowercase_name() {
    let msg = job_msg_with_endpoint("systemvssconnection", "AccessToken", "tok-2");
    assert_eq!(system_vss_access_token(&msg).as_deref(), Some("tok-2"));
  }

  #[test]
  fn lookup_finds_uppercase_name() {
    let msg = job_msg_with_endpoint("SYSTEMVSSCONNECTION", "AccessToken", "tok-3");
    assert_eq!(system_vss_access_token(&msg).as_deref(), Some("tok-3"));
  }

  #[test]
  fn lookup_finds_lowercase_key() {
    let msg = job_msg_with_endpoint("SystemVssConnection", "accesstoken", "tok-4");
    assert_eq!(system_vss_access_token(&msg).as_deref(), Some("tok-4"));
  }

  #[test]
  fn lookup_returns_none_for_missing_endpoint() {
    let msg = job_msg_with_endpoint("SomeOtherEndpoint", "AccessToken", "tok-5");
    assert_eq!(system_vss_access_token(&msg), None);
  }

  #[test]
  fn lookup_returns_none_for_missing_key() {
    let msg = job_msg_with_endpoint("SystemVssConnection", "DifferentKey", "tok-6");
    assert_eq!(system_vss_access_token(&msg), None);
  }
}
