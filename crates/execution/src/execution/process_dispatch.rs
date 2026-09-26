//! Merge process stdout and stderr into command and matcher dispatch before reporting.

use super::command_dispatch::{CommandDispatcher, LineDisposition};
use super::context::ExecutionContext;
use shared::{Conclusion, LogStream, RunnerEvent};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;

/// Process-local command effects and command failure status.
pub(super) struct ProcessOutput {
  pub(super) outputs: HashMap<String, String>,
  failed: bool,
}

impl ProcessOutput {
  /// Command failures override success while preserving cancellation.
  pub(super) fn conclusion(&self, conclusion: Conclusion) -> Conclusion {
    if self.failed && conclusion == Conclusion::Success {
      Conclusion::Failure
    } else {
      conclusion
    }
  }
}

/// Drain both process channels; callers close the event sender when execution ends.
pub(super) async fn stream_process(
  dispatcher: CommandDispatcher,
  stdout: &mut mpsc::Receiver<String>,
  process_events: &mut mpsc::Receiver<RunnerEvent>,
  ctx: &mut ExecutionContext,
  events: &mpsc::Sender<RunnerEvent>,
) -> ProcessOutput {
  let mut dispatcher = dispatcher;
  let mut stdout_open = true;
  let mut events_open = true;
  while stdout_open || events_open {
    let received = tokio::select! {
      line = stdout.recv(), if stdout_open => if let Some(line) = line {
        Some((line, LogStream::Stdout))
      } else {
        stdout_open = false;
        None
      },
      event = process_events.recv(), if events_open => match event {
        Some(RunnerEvent::Log { line, stream, .. }) => Some((line, stream)),
        Some(event) => { let _ = events.send(event).await; None },
        None => { events_open = false; None }
      }
    };
    if let Some((line, stream)) = received {
      let disposition = dispatcher.on_output_line(&line, stream, ctx);
      for event in dispatcher.take_events() {
        let _ = events.send(event).await;
      }
      if let LineDisposition::PassThrough(line) = disposition {
        let _ = events.send(dispatcher.output_log(line, stream)).await;
      }
    }
  }
  ProcessOutput {
    failed: dispatcher.command_failed(),
    outputs: dispatcher.take_set_outputs().into_iter().collect(),
  }
}

/// Bind the dispatcher to the action context and the enclosing report step.
pub(super) fn dispatcher(
  step: &str,
  report: &str,
  output: Option<&str>,
  ctx: &ExecutionContext,
) -> CommandDispatcher {
  CommandDispatcher::new(step, report, output, Arc::clone(ctx.masker()))
}
