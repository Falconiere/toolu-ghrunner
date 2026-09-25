//! Build and send the Run Service completion request.

use shared::RunnerError;
use wire::reporting::run_service::{CompleteJobOutput, CompleteJobRequest, complete_job};

use super::JobOutcome;
use crate::SessionCtx;
use crate::helpers::map_conclusion;

/// Report a finished job, retrying transient Run Service failures.
pub(super) async fn report_completion(
  ctx: &SessionCtx,
  run_service_url: &str,
  plan_id: String,
  rs_token: String,
  outcome: JobOutcome,
) -> Result<(), RunnerError> {
  let request = CompleteJobRequest {
    plan_id,
    job_id: outcome.job_id,
    request_id: outcome.request_id,
    conclusion: map_conclusion(outcome.conclusion),
    outputs: outcome
      .outputs
      .into_iter()
      .map(|(name, value)| (name, CompleteJobOutput { value }))
      .collect(),
    step_results: outcome.step_results,
    annotations: outcome.annotations,
  };
  // Borrow the payload across retries instead of cloning its step results.
  crate::retry::retry_transient(
    || complete_job(&ctx.client, run_service_url, &rs_token, &request),
    &ctx.cancel,
    crate::retry::REPORT_RETRY_MAX,
    "complete_job",
  )
  .await
}
