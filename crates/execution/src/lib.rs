//! Job execution engine: context, steps runner, handlers, expressions
//! glue, plus the Node.js runtime, bollard Docker wrapper, and the plugin
//! registry. Exposes the reusable [`Runner`] that spawns job work and
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
/// `RunnerPlugin` trait and registry.
pub mod plugin;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use shared::{AgentJobRequestMessage, Conclusion, LogStream, RunnerConfig, RunnerEvent};
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
    let (tx, rx) = mpsc::channel(1024);
    let config = self.config.clone();
    let masker = Arc::clone(&self.masker);

    tokio::spawn(async move {
      // `err_tx` exists only to report a failed job after `run_job` has
      // consumed the sender it was given. `job_id` is captured before the
      // same move, since a `run_job` error drops `job` before returning.
      let err_tx = tx.clone();
      let job_id = job.job_id.clone();
      let teardown =
        match crate::execution::job_runner::run_job(job, &config, cancel, tx, masker).await {
          Ok(teardown) => teardown,
          Err(err) => {
            tracing::error!(error = %err, "job execution failed");
            // Surface the failure in the GitHub job log too — `tracing::error!`
            // above only reaches the local diag log — before the completion
            // event, and report the real job id so the run/step actually
            // resolves out of "in_progress" on GitHub's side.
            // These are the last-resort diagnostics for a dead job, so a
            // closed channel is worth a warn rather than the crate's usual
            // silent best-effort send.
            if err_tx
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
            if err_tx
              .send(RunnerEvent::JobCompleted {
                job_id,
                conclusion: Conclusion::Failure,
                outputs: HashMap::new(),
              })
              .await
              .is_err()
            {
              tracing::warn!("event channel closed; job-failure completion event was dropped");
            }
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
