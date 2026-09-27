//! Serial summary upload queue, drained before job completion.

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::step_report_queue::StepReportQueueConfig;

/// Bounded immutable documents waiting for Results Service upload.
pub(crate) struct StepSummaryQueue {
  tx: mpsc::Sender<(String, String)>,
  handle: JoinHandle<()>,
}

impl StepSummaryQueue {
  /// Start one serial uploader; per-request deadlines are enforced by wire.
  pub(crate) fn spawn(cfg: StepReportQueueConfig) -> Self {
    // Documents may be MiB-sized; keep this smaller than the status-only queue.
    let (tx, mut rx) = mpsc::channel::<(String, String)>(16);
    let handle = tokio::spawn(async move {
      while let Some((step_id, content)) = rx.recv().await {
        if let Err(error) = wire::net::step_summary::upload_step_summary(
          &cfg.client,
          &cfg.results_url,
          &cfg.token,
          &wire::net::step_summary::StepSummary {
            run_backend_id: &cfg.run_backend_id,
            job_backend_id: &cfg.job_backend_id,
            step_backend_id: &step_id,
            content: content.as_bytes(),
          },
        )
        .await
        {
          // The transport returns safe stage/status diagnostics, never bodies
          // or signed URLs. One rejected document cannot stop later uploads.
          tracing::warn!(step_id, error = %error, "step summary upload failed");
        }
      }
    });
    Self { tx, handle }
  }

  /// Apply backpressure to bound memory without discarding completed summaries.
  pub(crate) async fn enqueue(&self, step_id: &str, content: String) {
    if self.tx.send((step_id.to_owned(), content)).await.is_err() {
      tracing::warn!("step summary queue closed before document was enqueued");
    }
  }

  /// Close and drain all documents, including after a job is cancelled.
  pub(crate) async fn drain(self) {
    drop(self.tx);
    if self.handle.await.is_err() {
      tracing::warn!("step summary uploader terminated before completing its queue");
    }
  }
}

#[cfg(test)]
#[path = "tests/step_summary.rs"]
mod tests;
