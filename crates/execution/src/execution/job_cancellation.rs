//! `request` starts one five-minute budget shared by running work and main/post cleanup.
//! `watch_step` re-evaluates its saved scope with cancelled job status: false/error
//! interrupts that step, while true may continue until the shared `force` fires.
//! `force` is a child of `shutdown` and also fires at the fixed job deadline.
//! Shutdown thus interrupts even `always()`; before a graceful request, it also
//! cancels the job-local request token to interrupt container setup and hooks.
//! Dropping controllers aborts watchers; subprocess owners still kill/reap children.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use expressions::evaluator::{EvalContext, JobStatus, evaluate};
use expressions::types::ExprValue;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// One budget shared by running work, subsequent cleanup and action posts.
const CLEANUP_GRACE: Duration = Duration::from_secs(300);

/// Job-scoped signals; dropping the controller stops its deadline task.
pub(crate) struct JobCancellation {
  /// Graceful job cancellation, also fired when shutdown interrupts setup.
  pub(crate) request: CancellationToken,
  /// Runner lifetime signal, which takes precedence over graceful cancellation.
  pub(crate) shutdown: CancellationToken,
  /// Interrupts every step when shutdown or the shared cleanup deadline fires.
  pub(crate) force: CancellationToken,
  deadline: Arc<OnceLock<Instant>>,
  watcher: JoinHandle<()>,
}

impl JobCancellation {
  /// Start the job cancellation deadline watcher.
  pub(crate) fn new(request: CancellationToken, shutdown: CancellationToken) -> Arc<Self> {
    Self::with_grace(request, shutdown, CLEANUP_GRACE)
  }

  fn with_grace(
    request: CancellationToken,
    shutdown: CancellationToken,
    grace: Duration,
  ) -> Arc<Self> {
    if shutdown.is_cancelled() {
      request.cancel();
    }
    let deadline = Arc::new(OnceLock::new());
    // Observe an already-fired signal synchronously, before any step can start.
    if request.is_cancelled() {
      let _ = deadline.set(Instant::now() + grace);
    }
    let force = shutdown.child_token();
    let stop = force.clone();
    let signal = request.clone();
    let clock = Arc::clone(&deadline);
    let watcher = tokio::spawn(async move {
      tokio::select! {
        () = stop.cancelled() => { signal.cancel(); return; },
        () = signal.cancelled() => {},
      }
      let at = *clock.get_or_init(|| Instant::now() + grace);
      tokio::select! {
        () = stop.cancelled() => {},
        () = tokio::time::sleep_until(at) => stop.cancel(),
      }
    });
    Arc::new(Self {
      request,
      shutdown,
      force,
      deadline,
      watcher,
    })
  }

  /// Absolute shared cleanup deadline, once job cancellation is observed.
  pub(crate) fn deadline(&self) -> Option<Instant> {
    self.deadline.get().copied()
  }

  /// Whether user work must stop even when its condition is still true.
  /// Latches `force` on shutdown or deadline expiry, cancelling all step children.
  /// This check can therefore stop remaining main/post work before the watcher runs.
  pub(crate) fn is_forced(&self) -> bool {
    if self.shutdown.is_cancelled() || self.deadline().is_some_and(|at| at <= Instant::now()) {
      self.force.cancel();
    }
    self.force.is_cancelled()
  }

  /// Resolve live cancellation/shutdown over the aggregate execution status.
  pub(crate) fn status(&self, prior: JobStatus) -> JobStatus {
    if self.shutdown.is_cancelled() {
      JobStatus::Failure
    } else if self.request.is_cancelled() {
      JobStatus::Cancelled
    } else {
      prior
    }
  }

  /// Keep the running step unless its condition becomes false on cancellation.
  /// The snapshot is the step's expression scope; only live job status changes.
  pub(crate) fn watch_step(&self, mut eval: EvalContext, condition: &str) -> StepCancellation {
    let cancel = self.force.child_token();
    let request = self.request.clone();
    let child = cancel.clone();
    let condition = if condition.trim().is_empty() {
      "success()"
    } else {
      condition
    }
    .to_owned();
    let watcher = tokio::spawn(async move {
      tokio::select! {
        () = child.cancelled() => return,
        () = request.cancelled() => {},
      }
      eval.job_status = JobStatus::Cancelled;
      if let Some(ExprValue::Object(job)) = eval.contexts.get_mut("job") {
        job.insert(
          "status".to_owned(),
          ExprValue::String("cancelled".to_owned()),
        );
      }
      match evaluate(&condition, &eval) {
        Ok(value) if value.is_truthy() => {},
        Ok(_) => child.cancel(),
        Err(error) => {
          tracing::warn!(%error, "running condition failed on cancellation");
          child.cancel();
        },
      }
    });
    StepCancellation {
      cancel,
      watcher: Some(watcher),
    }
  }
}

impl Drop for JobCancellation {
  fn drop(&mut self) {
    self.watcher.abort();
  }
}

/// Cancels the condition watcher when its step is finished, without detaching it.
pub(crate) struct StepCancellation {
  /// Per-step token controlled by the condition watcher and forced cancellation.
  pub(crate) cancel: CancellationToken,
  watcher: Option<JoinHandle<()>>,
}

impl Drop for StepCancellation {
  fn drop(&mut self) {
    if let Some(watcher) = &self.watcher {
      watcher.abort();
    }
  }
}

#[cfg(test)]
#[path = "tests/job_cancellation.rs"]
mod tests;
