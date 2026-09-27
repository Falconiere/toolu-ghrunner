//! Adapts setup metadata and preparation logs to the existing step pipeline.

use super::{ForwarderState, FwdConfig, StepCollector, handle_event_arm, report_step};
use shared::{Conclusion, ListenerEvent, RunnerEvent};
use std::collections::HashMap;
use tokio::sync::mpsc;

/// Start the setup log even when the Results Service is absent or unavailable.
pub(super) async fn start(
  state: &mut ForwarderState,
  cfg: &FwdConfig,
  collector: &StepCollector,
  tx: &mpsc::Sender<ListenerEvent>,
  lines: Vec<String>,
) {
  let Some(id) = &cfg.setup_id else {
    return;
  };
  let event = RunnerEvent::StepStarted {
    step_id: id.clone(),
    step_name: "Set up job".to_owned(),
    step_number: 1,
  };
  deliver(state, cfg, collector, tx, event).await;
  for line in lines {
    let event = RunnerEvent::Log {
      step_id: id.clone(),
      line,
      stream: shared::LogStream::Stdout,
    };
    deliver(state, cfg, collector, tx, event).await;
  }
}

/// Close initial setup before the first executable step, preserving early failures.
pub(super) async fn before_event(
  state: &mut ForwarderState,
  cfg: &FwdConfig,
  collector: &StepCollector,
  tx: &mpsc::Sender<ListenerEvent>,
  event: &mut RunnerEvent,
) {
  match event {
    RunnerEvent::StepStarted { .. } => {
      let conclusion = if cfg.setup_cancel.is_cancelled() {
        Conclusion::Cancelled
      } else {
        Conclusion::Success
      };
      finish(state, cfg, collector, tx, conclusion).await;
    },
    RunnerEvent::JobCompleted { conclusion, .. } => {
      finish(state, cfg, collector, tx, *conclusion).await;
    },
    RunnerEvent::Log { step_id, .. } => {
      if (step_id == shared::SETUP_STEP_ID || state.setup_pending)
        && let Some(id) = &cfg.setup_id
      {
        step_id.clone_from(id);
      }
    },
    RunnerEvent::JobStarted { .. }
    | RunnerEvent::StepCompleted { .. }
    | RunnerEvent::StepSkipped { .. }
    | RunnerEvent::LogGroup { .. }
    | RunnerEvent::Annotation { .. } => {},
  }
}

/// Complete setup once; retain its uploader for deferred and nested downloads.
pub(super) async fn finish(
  state: &mut ForwarderState,
  cfg: &FwdConfig,
  collector: &StepCollector,
  tx: &mpsc::Sender<ListenerEvent>,
  conclusion: Conclusion,
) {
  if !std::mem::replace(&mut state.setup_pending, false) {
    return;
  }
  let Some(id) = &cfg.setup_id else {
    return;
  };
  let event = RunnerEvent::StepCompleted {
    step_id: id.clone(),
    conclusion,
    outputs: HashMap::new(),
  };
  deliver(state, cfg, collector, tx, event).await;
}

async fn deliver(
  state: &mut ForwarderState,
  cfg: &FwdConfig,
  collector: &StepCollector,
  tx: &mpsc::Sender<ListenerEvent>,
  event: RunnerEvent,
) {
  collector.record(&event).await;
  handle_event_arm(state, cfg, &event).await;
  // The initial in-progress update already ran alongside the WS handshake.
  if matches!(event, RunnerEvent::StepStarted { .. }) {
    let _ = crate::step_report_queue::build_step_entry(&event, &mut state.step_meta);
  } else {
    report_step(state, &event);
  }
  if tx.send(ListenerEvent::Runner(event)).await.is_err() {
    tracing::warn!("setup journal event receiver closed");
  }
}
