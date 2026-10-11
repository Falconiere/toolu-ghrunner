//! Finalize job outputs inside the "Complete job" step, after the main and
//! post step loops (upstream `JobExtension.FinalizeJob`).

use std::collections::HashMap;

use shared::{AnnotationLevel, Conclusion, RunnerError};

use super::complete_step::CompleteStep;
use crate::execution::context::ExecutionContext;
use crate::execution::job_spec::{
  JobSpec, NULL_TOKEN, evaluate_acquired_outputs, evaluate_job_outputs,
};

/// Resolved job outputs and whether their evaluation failed.
pub(super) struct FinalOutputs {
  /// Job conclusion after an output evaluation failure (cancellation wins).
  pub(super) conclusion: Conclusion,
  /// Nonempty, secret-free outputs.
  pub(super) outputs: HashMap<String, String>,
  /// An output failed to evaluate, which fails the "Complete job" step.
  pub(super) failed: bool,
}

pub(super) async fn evaluate_final_outputs(
  spec: &JobSpec,
  ctx: &ExecutionContext,
  step: &CompleteStep<'_>,
  conclusion: Conclusion,
) -> Result<FinalOutputs, RunnerError> {
  let Some(token) = spec
    .acquired_outputs
    .as_ref()
    .filter(|t| t.token_type != NULL_TOKEN)
  else {
    return Ok(FinalOutputs {
      conclusion,
      outputs: evaluate_job_outputs(spec, ctx)?,
      failed: false,
    });
  };
  step.log("Evaluate and set job outputs").await;
  let evaluated = evaluate_acquired_outputs(token, ctx);
  for name in &evaluated.skipped_secret_names {
    let safe_name = step.mask(name);
    step
      .issue(
        AnnotationLevel::Warning,
        &format!("Skip output '{safe_name}' since it may contain secret."),
      )
      .await;
  }
  let mut names: Vec<&String> = evaluated.outputs.keys().collect();
  names.sort();
  for name in names {
    let safe_name = step.mask(name);
    step.log(&format!("Set output '{safe_name}'")).await;
  }
  let failed = evaluated.error.is_some();
  if let Some(error) = &evaluated.error {
    tracing::error!("job output evaluation failed");
    step
      .issue(AnnotationLevel::Error, "Fail to evaluate job outputs")
      .await;
    step
      .issue(AnnotationLevel::Error, &step.mask(&error.to_string()))
      .await;
  }
  let conclusion = if failed && conclusion != Conclusion::Cancelled {
    Conclusion::Failure
  } else {
    conclusion
  };
  Ok(FinalOutputs {
    conclusion,
    outputs: evaluated.outputs,
    failed,
  })
}
