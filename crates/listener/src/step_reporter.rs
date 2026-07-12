//! Collects step results from RunnerEvents for inclusion in complete_job.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Mutex;

use super::helpers::map_conclusion;
use wire::reporting::{Status, StepResult};
use shared::RunnerEvent;

/// Per-step metadata captured from StepStarted.
struct CollectedMeta {
  number: u32,
  name: String,
  started_at: String,
}

/// Inner state behind the Arc<Mutex>.
struct CollectorState {
  meta: HashMap<String, CollectedMeta>,
  results: Vec<StepResult>,
}

/// Collects step results during job execution.
#[derive(Clone)]
pub(super) struct StepCollector {
  state: Arc<Mutex<CollectorState>>,
}

impl StepCollector {
  pub(super) fn new() -> Self {
    Self {
      state: Arc::new(Mutex::new(CollectorState {
        meta: HashMap::new(),
        results: Vec::new(),
      })),
    }
  }

  /// Record a step event. Captures metadata on StepStarted, builds result on StepCompleted.
  pub(super) async fn record(&self, event: &RunnerEvent) {
    match event {
      RunnerEvent::StepStarted {
        step_id,
        step_name,
        step_number,
      } => {
        self.state.lock().await.meta.insert(
          step_id.clone(),
          CollectedMeta {
            number: *step_number,
            name: step_name.clone(),
            started_at: chrono::Utc::now().to_rfc3339(),
          },
        );
      },
      RunnerEvent::StepCompleted {
        step_id,
        conclusion,
        ..
      } => {
        let c = map_conclusion(*conclusion);
        let mut state = self.state.lock().await;
        let meta = state.meta.remove(step_id);
        let (number, name, started_at) = match meta {
          Some(m) => (m.number, m.name, Some(m.started_at)),
          None => (0, String::new(), None),
        };
        state.results.push(StepResult {
          external_id: step_id.clone(),
          number,
          name,
          status: Status::Completed,
          conclusion: c,
          outcome: c,
          started_at,
          completed_at: Some(chrono::Utc::now().to_rfc3339()),
          completed_log_url: None,
          completed_log_lines: None,
        });
      },
      RunnerEvent::JobStarted { .. }
      | RunnerEvent::StepSkipped { .. }
      | RunnerEvent::Log { .. }
      | RunnerEvent::LogGroup { .. }
      | RunnerEvent::Annotation { .. }
      | RunnerEvent::JobCompleted { .. } => {},
    }
  }

  /// Append a pre-built StepResult (e.g. setup step).
  pub(super) async fn push_result(&self, result: StepResult) {
    self.state.lock().await.results.push(result);
  }

  /// Backfill log URL and line count onto an already-recorded step result.
  ///
  /// Called after background log upload completes. Finds the result by
  /// `external_id` and updates the log fields in place.
  pub(super) async fn set_log_url(&self, step_id: &str, url: String, line_count: u64) {
    let mut state = self.state.lock().await;
    if let Some(result) = state.results.iter_mut().find(|r| r.external_id == step_id) {
      result.completed_log_url = Some(url);
      result.completed_log_lines = Some(line_count);
    }
  }

  /// Return all collected step results.
  pub(super) async fn collected_results(&self) -> Vec<StepResult> {
    self.state.lock().await.results.clone()
  }
}
