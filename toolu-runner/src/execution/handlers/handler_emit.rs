//! Shared event emission helpers for built-in action handlers.
//!
//! `#[allow(dead_code)]` — these helpers are `pub(super)` and only consumed
//! by sibling handler modules that land in step 4d. The build is green now
//! because the functions are inert until the handlers use them.

#![allow(dead_code)]

use shared::{ActionStep, LogStream, RunnerEvent};
use tokio::sync::mpsc;

pub(super) async fn log(step: &ActionStep, events: &mpsc::Sender<RunnerEvent>, msg: &str) {
  let _ = events
    .send(RunnerEvent::Log {
      step_id: step.id.clone(),
      line: msg.to_owned(),
      stream: LogStream::Stdout,
    })
    .await;
}

pub(super) async fn error(step: &ActionStep, events: &mpsc::Sender<RunnerEvent>, msg: &str) {
  let _ = events
    .send(RunnerEvent::Log {
      step_id: step.id.clone(),
      line: msg.to_owned(),
      stream: LogStream::Stderr,
    })
    .await;
}

pub(super) async fn group_open(step: &ActionStep, events: &mpsc::Sender<RunnerEvent>, title: &str) {
  let _ = events
    .send(RunnerEvent::LogGroup {
      step_id: step.id.clone(),
      title: title.to_owned(),
      open: true,
    })
    .await;
}

pub(super) async fn group_close(step: &ActionStep, events: &mpsc::Sender<RunnerEvent>) {
  let _ = events
    .send(RunnerEvent::LogGroup {
      step_id: step.id.clone(),
      title: String::new(),
      open: false,
    })
    .await;
}
