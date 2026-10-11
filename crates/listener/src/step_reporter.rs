//! Collects step results from `RunnerEvent`s for inclusion in `complete_job`.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Mutex;

use shared::{AnnotationLevel, RunnerEvent};
use wire::reporting::{Annotation, ReportAnnotationLevel, StepResult, StepState};

/// Per-step metadata captured from `StepStarted`.
struct CollectedMeta {
  number: u32,
  name: String,
  started_at: String,
}

/// A step's upstream telemetry: `type`, `action_name`, `ref`.
type ActionIdentity = (String, Option<String>, Option<String>);

/// Inner state behind the Arc<Mutex>.
struct CollectorState {
  meta: HashMap<String, CollectedMeta>,
  pending_annotations: HashMap<String, Vec<Annotation>>,
  /// Action identity per step id, applied when the step's result is built.
  identities: HashMap<String, ActionIdentity>,
  /// Infrastructure categories reported per step, latched on failure.
  infrastructure: HashMap<String, String>,
  /// First category of a step that ended failed (upstream: first wins).
  category: Option<String>,
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
        identities: HashMap::new(),
        infrastructure: HashMap::new(),
        category: None,
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
      RunnerEvent::StepMetadata {
        step_id,
        kind,
        action,
        git_ref,
      } => {
        let identity = (kind.clone(), action.clone(), git_ref.clone());
        self
          .state
          .lock()
          .await
          .identities
          .insert(step_id.clone(), identity);
      },
      RunnerEvent::InfrastructureError {
        step_id, category, ..
      } => {
        self
          .state
          .lock()
          .await
          .infrastructure
          .insert(step_id.clone(), category.clone());
        self.record_annotation(step_id, event).await;
      },
      RunnerEvent::StepSummary { .. }
      | RunnerEvent::JobStarted { .. }
      | RunnerEvent::StepSkipped { .. }
      | RunnerEvent::Log { .. }
      | RunnerEvent::LogGroup { .. }
      | RunnerEvent::JobCompleted { .. } => {},
    }
  }

  async fn record_completion(&self, step_id: &str, conclusion: shared::Conclusion) {
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
    let infrastructure = state.infrastructure.remove(step_id);
    // A continue-on-error step that turns green reports its annotation but
    // no job category: a successful job never claims an infrastructure fault.
    if conclusion == shared::Conclusion::Failure && state.category.is_none() {
      state.category = infrastructure;
    }
    let (kind, action_name, git_ref) = match state.identities.remove(step_id) {
      Some((kind, action, git_ref)) => (Some(kind), action, git_ref),
      None => (None, None, None),
    };
    state.results.push(StepResult {
      external_id: step_id.to_owned(),
      number,
      name,
      action_name,
      git_ref,
      kind,
      status: StepState::Completed,
      conclusion: conclusion.into(),
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

  /// Category of the first infrastructure failure on a step that ended failed.
  pub(super) async fn infrastructure_category(&self) -> Option<String> {
    self.state.lock().await.category.clone()
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
  let step_number = number.map_or(0, i64::from);
  if let RunnerEvent::InfrastructureError { message, .. } = event {
    // Upstream `InfrastructureError`: an error issue flagged as the runner's.
    return Some(Annotation {
      level: ReportAnnotationLevel::Failure,
      message: message.clone(),
      title: None,
      raw_details: None,
      path: None,
      is_infrastructure_issue: true,
      start_line: 0,
      end_line: 0,
      start_column: 0,
      end_column: 0,
      step_number,
    });
  }
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
    step_number,
  })
}
