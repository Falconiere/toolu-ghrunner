//! `StepLogStreamer` — per-step log streaming actor with gzip final upload.
//!
//! Spawns a tokio task that receives log lines via an mpsc channel
//! and uploads a gzip-compressed blob via Results Service on finalize.

use std::path::PathBuf;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::upload::upload_step_logs;
use crate::helpers::ResultsCtx;
use crate::log_spool::LogSpool;

/// Channel capacity for log lines sent to the streamer actor.
pub const CHANNEL_CAPACITY: usize = 4;

/// Configuration for spawning a `StepLogStreamer` actor.
pub struct StreamerConfig {
  /// HTTP client used for the Results Service upload.
  pub client: reqwest::Client,
  /// Base URL of the Results Service.
  pub results_url: String,
  /// Bearer token authorizing the upload.
  pub token: String,
  /// The job's run-level backend id.
  pub run_backend_id: String,
  /// The job's job-level backend id.
  pub job_backend_id: String,
  /// The step's backend id.
  pub step_backend_id: String,
  /// The step's display name.
  pub step_name: String,
  /// Private directory containing this job's log spools.
  pub spool_dir: PathBuf,
}

/// Spawn a `StepLogStreamer` actor. Returns the sender for log lines and a
/// join handle that resolves to `Some((logs_url, line_count))` on success.
pub fn spawn(cfg: StreamerConfig) -> (mpsc::Sender<String>, JoinHandle<Option<(String, u64)>>) {
  let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
  let handle = tokio::spawn(StepLogStreamer::new(cfg, rx).run());
  (tx, handle)
}

struct StepLogStreamer {
  cfg: StreamerConfig,
  lines_rx: mpsc::Receiver<String>,
  spool: Option<LogSpool>,
}

impl StepLogStreamer {
  fn new(cfg: StreamerConfig, lines_rx: mpsc::Receiver<String>) -> Self {
    Self {
      cfg,
      lines_rx,
      spool: None,
    }
  }

  async fn run(mut self) -> Option<(String, u64)> {
    if let Err(error) = self.run_loop().await {
      tracing::warn!(error = %error, "step log spool failed");
      return None;
    }
    self.finalize().await
  }

  async fn run_loop(&mut self) -> std::io::Result<()> {
    while let Some(line) = self.lines_rx.recv().await {
      if self.spool.is_none() {
        self.spool = Some(LogSpool::for_step(&self.cfg.spool_dir)?);
      }
      if let Some(spool) = self.spool.as_mut() {
        spool.append(&line)?;
      }
    }
    Ok(())
  }

  async fn finalize(mut self) -> Option<(String, u64)> {
    let (path, count) = match self.spool.take()?.finish() {
      Ok(finished) => finished,
      Err(error) => {
        tracing::warn!(error = %error, "step log spool flush failed");
        return None;
      },
    };
    let rctx = ResultsCtx {
      client: &self.cfg.client,
      results_url: &self.cfg.results_url,
      token: &self.cfg.token,
      run_backend_id: &self.cfg.run_backend_id,
      job_backend_id: &self.cfg.job_backend_id,
    };
    upload_step_logs(&rctx, &self.cfg.step_backend_id, path, count).await
  }
}
