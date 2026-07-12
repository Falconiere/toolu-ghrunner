use wire::reporting::results_service::{
  StepUpdateEntry, WorkflowStepsUpdateRequest, update_workflow_steps,
};
use wire::reporting::{ReportConclusion, Status, StepResult};
use shared::AgentJobRequestMessage;

/// Report "Set up job" as step number 1 via Results Service.
///
/// Returns a `StepResult` for inclusion in `complete_job` and the log lines
/// for inclusion in the combined job-level log upload.
pub(super) async fn report_setup_step(
  token: &str,
  plan_id: &str,
  job_msg: &AgentJobRequestMessage,
  client: &reqwest::Client,
) -> (Option<StepResult>, Vec<String>) {
  let Some(results_url) = job_msg
    .variables
    .get("system.github.results_endpoint")
    .map(|v| v.value.trim_end_matches('/'))
  else {
    return (None, Vec::new());
  };

  let external_id = uuid::Uuid::new_v4().to_string();
  let now = chrono::Utc::now().to_rfc3339();
  let (run_backend_id, job_backend_id) = super::helpers::resolve_backend_ids(job_msg, plan_id);

  let request = WorkflowStepsUpdateRequest {
    steps: vec![StepUpdateEntry {
      external_id: external_id.clone(),
      number: 1,
      name: "Set up job".to_owned(),
      status: Status::Completed,
      conclusion: Some(ReportConclusion::Success),
      started_at: Some(now.clone()),
      completed_at: Some(now.clone()),
    }],
    change_order: 0,
    workflow_run_backend_id: run_backend_id.clone(),
    workflow_job_run_backend_id: job_backend_id.clone(),
  };

  if let Err(e) = update_workflow_steps(client, results_url, token, &request).await {
    tracing::warn!(error = %e, "setup step report failed");
    return (None, Vec::new());
  }

  // Upload log blob using the same external_id as step_backend_id
  let rctx = super::helpers::ResultsCtx {
    client,
    results_url,
    token,
    run_backend_id: &run_backend_id,
    job_backend_id: &job_backend_id,
  };
  let lines = vec!["Preparing runner...".to_owned(), "Runner ready.".to_owned()];
  let log_result =
    super::log_uploader::upload_compressed_step_logs(&rctx, &external_id, &lines).await;

  (
    Some(StepResult {
      external_id,
      number: 1,
      name: "Set up job".to_owned(),
      status: Status::Completed,
      conclusion: ReportConclusion::Success,
      outcome: ReportConclusion::Success,
      started_at: Some(now.clone()),
      completed_at: Some(now),
      completed_log_url: log_result.as_ref().map(|(url, _)| url.clone()),
      completed_log_lines: log_result.map(|(_, count)| count),
    }),
    lines,
  )
}
