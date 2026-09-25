//! Finalize job outputs after the main and post step loops.

use std::collections::HashMap;

use shared::{AnnotationLevel, Conclusion, RunnerError, RunnerEvent};
use tokio::sync::mpsc;

use crate::execution::context::ExecutionContext;
use crate::execution::job_spec::{JobSpec, evaluate_acquired_outputs, evaluate_job_outputs};

pub(super) async fn evaluate_final_outputs(
  spec: &JobSpec,
  ctx: &ExecutionContext,
  events: &mpsc::Sender<RunnerEvent>,
  job_id: &str,
  conclusion: Conclusion,
) -> Result<(Conclusion, HashMap<String, String>), RunnerError> {
  let Some(token) = &spec.acquired_outputs else {
    return Ok((conclusion, evaluate_job_outputs(spec, ctx)?));
  };
  let evaluated = evaluate_acquired_outputs(token, ctx);
  for name in &evaluated.skipped_secret_names {
    emit_secret_warning(name, ctx, events, job_id).await;
  }
  let conclusion = if evaluated.error.is_some() && conclusion != Conclusion::Cancelled {
    tracing::error!("job output evaluation failed");
    Conclusion::Failure
  } else {
    conclusion
  };
  Ok((conclusion, evaluated.outputs))
}

async fn emit_secret_warning(
  name: &str,
  ctx: &ExecutionContext,
  events: &mpsc::Sender<RunnerEvent>,
  job_id: &str,
) {
  let safe_name = match ctx.masker().lock() {
    Ok(guard) => guard.mask(name).into_owned(),
    Err(poisoned) => poisoned.into_inner().mask(name).into_owned(),
  };
  let message = format!("Skip output '{safe_name}' since it may contain a secret");
  tracing::warn!("{message}");
  if events
    .send(RunnerEvent::Annotation {
      step_id: job_id.to_owned(),
      level: AnnotationLevel::Warning,
      message,
      file: None,
      line: None,
    })
    .await
    .is_err()
  {
    tracing::warn!("job output annotation receiver dropped before delivery");
  }
}
