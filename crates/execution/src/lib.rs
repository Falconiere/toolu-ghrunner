//! Job execution engine: context, steps runner, handlers, expressions
//! glue, plus the Node.js runtime, and bollard Docker wrapper.
//! Exposes the reusable [`Runner`] that spawns job work and
//! returns an event stream. Depends only on `shared`, `expressions`, and
//! `cache` — never on the listener, `wire`, or `observability`.

/// Environment-derived configuration (the only allowed `std::env::var` site).
pub mod config;
/// Bollard wrapper: daemon client, service containers, path translation.
pub mod docker;
/// Job execution engine (context, steps runner, handlers, expressions).
pub mod execution;
/// Node.js runtime detection, download, and caching.
pub mod node;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use shared::{
  AgentJobRequestMessage, Conclusion, LogStream, RunnerConfig, RunnerError, RunnerEvent,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub use shared::RunnerConfig as Config;

/// Reusable execution engine. Spawns job work and returns an event stream.
#[derive(Debug, Clone)]
pub struct Runner {
  config: RunnerConfig,
  /// Shared with the listener and the tracing file sink's redactor.
  /// The `ExecutionContext` for each job receives the same Arc, so
  /// `register_secret` and `add_mask` propagate to all readers on the
  /// next call.
  masker: Arc<Mutex<shared::SecretMasker>>,
}

impl Runner {
  /// Create a runner bound to a config and a shared secret masker.
  pub fn new(config: RunnerConfig, masker: Arc<Mutex<shared::SecretMasker>>) -> Self {
    Self { config, masker }
  }

  /// Execute a single job. Returns a receiver for the event stream.
  ///
  /// The job runs in a background task. Events are emitted as the job
  /// progresses. The stream closes when the job completes.
  ///
  /// **The event stream is unmasked.** `RunnerEvent::Log` lines are not
  /// passed through the secret masker; every consumer of this receiver must
  /// mask before writing a line to a durable sink. `execution` is a public
  /// library, so callers outside this workspace must honor this contract too.
  pub fn execute_job(
    &self,
    job: AgentJobRequestMessage,
    cancel: CancellationToken,
  ) -> mpsc::Receiver<RunnerEvent> {
    self.execute_job_with_shutdown(job, cancel, CancellationToken::new())
  }

  /// Execute with distinct GitHub job cancellation and runner shutdown signals.
  /// Shutdown interrupts every condition and concludes the job as failure.
  pub fn execute_job_with_shutdown(
    &self,
    job: AgentJobRequestMessage,
    cancel: CancellationToken,
    shutdown: CancellationToken,
  ) -> mpsc::Receiver<RunnerEvent> {
    // Log lines may be many MiB each; keep queued events bounded in bytes too.
    let (tx, rx) = mpsc::channel(64);
    let config = self.config.clone();
    let masker = Arc::clone(&self.masker);

    tokio::spawn(async move {
      // `err_tx` exists only to report a failed job after `run_job` has
      // consumed the sender it was given. `job_id` is captured before the
      // same move, since a `run_job` error drops `job` before returning.
      let err_tx = tx.clone();
      let job_id = job.job_id.clone();
      let teardown = match crate::execution::job_runner::run_job_with_shutdown(
        job, &config, cancel, tx, masker, shutdown,
      )
      .await
      {
        Ok(teardown) => teardown,
        Err(err) => {
          tracing::error!(error = %err, "job execution failed");
          report_job_failure(&err_tx, job_id, &err).await;
          return;
        },
      };
      // Dropping the last sender closes the event channel, which is what
      // ultimately reports the job to GitHub. Cache GC and the workspace
      // sweep then overlap that round-trip instead of preceding it.
      drop(err_tx);
      teardown.finish(&config).await;
    });

    rx
  }

  /// Borrow the runner's config.
  pub fn config(&self) -> &RunnerConfig {
    &self.config
  }

  /// Borrow the shared secret masker.
  pub fn masker(&self) -> &Arc<Mutex<shared::SecretMasker>> {
    &self.masker
  }
}

/// Surface an engine error in the job stream so the remote job can finish.
async fn report_job_failure(tx: &mpsc::Sender<RunnerEvent>, job_id: String, err: &RunnerError) {
  // These are last-resort diagnostics, so a closed channel merits a warning.
  if tx
    .send(RunnerEvent::Log {
      step_id: String::new(),
      line: format!("##[error]{err}"),
      stream: LogStream::Stdout,
    })
    .await
    .is_err()
  {
    tracing::warn!("event channel closed; job-failure log line was dropped");
  }
  if tx
    .send(RunnerEvent::JobCompleted {
      job_id,
      conclusion: Conclusion::Failure,
      outputs: HashMap::new(),
      environment_url: None,
    })
    .await
    .is_err()
  {
    tracing::warn!("event channel closed; job-failure completion event was dropped");
  }
}
