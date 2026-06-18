//! GitHub Actions JIT listener, execution engine, and CLI binary.
//!
//! Module map:
//! - [`net`] — async network layer (token exchange, session, job lifecycle).
//! - [`listener`] — GitHub JIT lifecycle (handler, job execution loop).
//! - [`reporting`] — run service, results service, log upload, timeline.
//! - [`execution`] — job execution engine (context, steps runner, handlers).
//! - [`docker`] — bollard wrapper, service containers, path translation.
//! - [`node`] — Node.js runtime detection and caching.
//! - [`plugin`] — `RunnerPlugin` trait and registry.
//! - [`lockfile`] — single-job file lock for the `.lock` next to the
//!   registration config; prevents two `run` processes from sharing one
//!   registration.
//! - [`config`] — persisted registration + runtime config (`config.toml`)
//!   and OAuth credentials (`credentials.json`); matches the
//!   `~/.toolu-runner/` storage layout from the spec.
//!
//! Populated progressively in steps 2–9 per the plan.

#![doc(html_root_url = "https://docs.rs/toolu-runner/0.1.0")]

pub mod config;
pub mod docker;
pub mod execution;
pub mod listener;
pub mod lockfile;
pub mod net;
pub mod node;
pub mod plugin;
pub mod reporting;

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
  masker: Arc<Mutex<execution::secret_masker::SecretMasker>>,
}

impl Runner {
  /// Create a runner bound to a config and a shared secret masker.
  pub fn new(
    config: RunnerConfig,
    masker: Arc<Mutex<execution::secret_masker::SecretMasker>>,
  ) -> Self {
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
  pub fn masker(&self) -> &Arc<Mutex<execution::secret_masker::SecretMasker>> {
    &self.masker
  }
}
