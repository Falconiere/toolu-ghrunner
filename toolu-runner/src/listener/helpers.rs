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
