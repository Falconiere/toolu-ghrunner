//! StepLogStreamer — per-step log streaming actor with gzip final upload.
//!
//! Spawns a tokio task that receives log lines via an mpsc channel
//! and uploads a gzip-compressed blob via Results Service on finalize.

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::upload::upload_compressed_step_logs;
use crate::listener::helpers::ResultsCtx;

/// Channel capacity for log lines sent to the streamer actor.
pub const CHANNEL_CAPACITY: usize = 4096;

/// Configuration for spawning a `StepLogStreamer` actor.
pub struct StreamerConfig {
  pub client: reqwest::Client,
  pub results_url: String,
  pub token: String,
  pub run_backend_id: String,
  pub job_backend_id: String,
  pub step_backend_id: String,
  pub step_name: String,
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
  all_lines: Vec<String>,
}

impl StepLogStreamer {
  fn new(cfg: StreamerConfig, lines_rx: mpsc::Receiver<String>) -> Self {
    Self {
      cfg,
      lines_rx,
      all_lines: Vec::new(),
    }
  }

  async fn run(mut self) -> Option<(String, u64)> {
    self.run_loop().await;
    self.finalize().await
  }

  async fn run_loop(&mut self) {
    while let Some(line) = self.lines_rx.recv().await {
      self.all_lines.push(line);
    }
  }

  async fn finalize(&self) -> Option<(String, u64)> {
    let rctx = ResultsCtx {
      client: &self.cfg.client,
      results_url: &self.cfg.results_url,
      token: &self.cfg.token,
      run_backend_id: &self.cfg.run_backend_id,
      job_backend_id: &self.cfg.job_backend_id,
    };
    upload_compressed_step_logs(&rctx, &self.cfg.step_backend_id, &self.all_lines).await
  }
}
