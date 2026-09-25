//! Collects step results from `RunnerEvent`s for inclusion in `complete_job`.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Mutex;

use super::helpers::map_conclusion;
use shared::{AnnotationLevel, RunnerEvent};
use wire::reporting::{Annotation, ReportAnnotationLevel, Status, StepResult};

/// Per-step metadata captured from `StepStarted`.
struct CollectedMeta {
  number: u32,
  name: String,
  started_at: String,
}

/// Inner state behind the Arc<Mutex>.
struct CollectorState {
  meta: HashMap<String, CollectedMeta>,
  pending_annotations: HashMap<String, Vec<Annotation>>,
  results: Vec<StepResult>,
}

/// Collects step results during job execution.
#[derive(Clone)]
pub(super) struct StepCollector {
  state: Arc<Mutex<CollectorState>>,
}

impl StepCollector {
  /// Create an empty collector for one acquired job.
  pub(super) fn new() -> Self {
    Self {
      state: Arc::new(Mutex::new(CollectorState {
        meta: HashMap::new(),
        pending_annotations: HashMap::new(),
        results: Vec::new(),
      })),
    }
  }

  /// Record a step event. Captures metadata on `StepStarted`, builds result on `StepCompleted`.
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
        self.record_completion(step_id, *conclusion).await;
      },
      RunnerEvent::Annotation { step_id, .. } => {
        self.record_annotation(step_id, event).await;
      },
      RunnerEvent::JobStarted { .. }
      | RunnerEvent::StepSkipped { .. }
      | RunnerEvent::Log { .. }
      | RunnerEvent::LogGroup { .. }
      | RunnerEvent::JobCompleted { .. } => {},
    }
  }

  async fn record_completion(&self, step_id: &str, conclusion: shared::Conclusion) {
    let c = map_conclusion(conclusion);
    let mut state = self.state.lock().await;
    let meta = state.meta.remove(step_id);
    let (number, name, started_at) = match meta {
      Some(m) => (m.number, m.name, Some(m.started_at)),
      None => (0, String::new(), None),
    };
    let annotations = state
      .pending_annotations
      .remove(step_id)
      .unwrap_or_default();
    state.results.push(StepResult {
      external_id: step_id.to_owned(),
      number,
      name,
      status: Status::Completed,
      conclusion: c,
      outcome: c,
      started_at,
      completed_at: Some(chrono::Utc::now().to_rfc3339()),
      completed_log_url: None,
      completed_log_lines: None,
      annotations,
    });
  }

  async fn record_annotation(&self, step_id: &str, event: &RunnerEvent) {
    let mut state = self.state.lock().await;
    let number = state.meta.get(step_id).map(|meta| meta.number).or_else(|| {
      state
        .results
        .iter()
        .find(|result| result.external_id == step_id)
        .map(|result| result.number)
    });
    let Some(annotation) = to_report_annotation(event, number) else {
      return;
    };
    if let Some(result) = state
      .results
      .iter_mut()
      .find(|result| result.external_id == step_id)
    {
      result.annotations.push(annotation);
    } else {
      state
        .pending_annotations
        .entry(step_id.to_owned())
        .or_default()
        .push(annotation);
    }
  }

  /// Append a pre-built `StepResult` (e.g. setup step).
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

  /// Return completed step results; omit annotations without a completed step.
  pub(super) async fn collected_results(&self) -> Vec<StepResult> {
    let state = self.state.lock().await;
    if !state.pending_annotations.is_empty() {
      tracing::warn!(
        steps = state.pending_annotations.len(),
        "annotations have no completed step result and will be omitted"
      );
    }
    state.results.clone()
  }
}

fn to_report_annotation(event: &RunnerEvent, number: Option<u32>) -> Option<Annotation> {
  let RunnerEvent::Annotation {
    level,
    message,
    file,
    line,
    end_line,
    col,
    end_column,
    title,
    ..
  } = event
  else {
    return None;
  };
  let level = match level {
    AnnotationLevel::Notice => ReportAnnotationLevel::Notice,
    AnnotationLevel::Warning => ReportAnnotationLevel::Warning,
    AnnotationLevel::Error => ReportAnnotationLevel::Failure,
  };
  Some(Annotation {
    level,
    message: message.clone(),
    title: title.clone(),
    raw_details: None,
    path: file.clone(),
    is_infrastructure_issue: false,
    start_line: line.map_or(0, i64::from),
    end_line: end_line.map_or(0, i64::from),
    start_column: col.map_or(0, i64::from),
    end_column: end_column.map_or(0, i64::from),
    step_number: number.map_or(0, i64::from),
  })
}
