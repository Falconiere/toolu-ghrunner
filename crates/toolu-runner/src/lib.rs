//! GitHub Actions JIT listener, execution engine, and CLI binary.

#![doc(html_root_url = "https://docs.rs/toolu-runner/0.1.0")]

/// Bollard wrapper: daemon client, service containers, path translation.
pub mod docker;
/// Job execution engine (context, steps runner, handlers, expressions).
pub mod execution;
/// GitHub JIT lifecycle: handler, poll loop, execution loop.
pub mod listener;
/// Node.js runtime detection, download, and caching.
pub mod node;
/// `RunnerPlugin` trait and registry.
pub mod plugin;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use shared::{AgentJobRequestMessage, Conclusion, RunnerConfig, RunnerEvent};
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
  pub async fn execute_job(
    &self,
    job: AgentJobRequestMessage,
    cancel: CancellationToken,
  ) -> mpsc::Receiver<RunnerEvent> {
    let (tx, rx) = mpsc::channel(1024);
    let config = self.config.clone();
    let masker = Arc::clone(&self.masker);

    tokio::spawn(async move {
      if let Err(err) =
        execution::job_runner::run_job(job, &config, cancel, tx.clone(), masker).await
      {
        tracing::error!(error = %err, "job execution failed");
        let _ = tx
          .send(RunnerEvent::JobCompleted {
            job_id: String::new(),
            conclusion: Conclusion::Failure,
            outputs: HashMap::new(),
          })
          .await;
      }
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
