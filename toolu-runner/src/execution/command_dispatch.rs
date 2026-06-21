//! Consumes GitHub Actions workflow commands from a step's stdout and
//! applies them to the live execution context.
//!
//! `command_parser::parse_command` produces `WorkflowCommand` values; this
//! module is the consumer that turns each parsed command into an effect:
//! step outputs/state, secret masks (via the **shared** `SecretMasker`),
//! env/path mutations, log groups, and annotations. Non-command lines (and
//! lines emitted while `stop-commands` is active) pass through verbatim so
//! they are logged unchanged.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use shared::{AnnotationLevel, RunnerEvent};
use tokio::sync::mpsc;

use super::command_parser::{WorkflowCommand, parse_command};
use super::context::ExecutionContext;
use super::secret_masker::SecretMasker;

/// What to do with a stdout line after the dispatcher has inspected it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineDisposition {
  /// The line was a workflow command and has been applied; do not log it.
  Consumed,
  /// The line is plain output; log it verbatim (already secret-masked
  /// when the caller masks, so this carries the original text).
  PassThrough(String),
}

/// Per-step consumer of stdout workflow commands.
///
/// Holds the **shared** `Arc<Mutex<SecretMasker>>` (the same instance the
/// listener registered as the tracing file-sink redactor), so an
/// `::add-mask::` applied here also redacts the `_diag` log sink — never a
/// private copy. Per-step mutable state (echo flag, `stop-commands` resume
/// token, group nesting depth) lives here and resets with each new step.
pub struct CommandDispatcher {
  masker: Arc<Mutex<SecretMasker>>,
  step_id: String,
  echo_on: bool,
  /// When `Some(token)`, command processing is suspended until a line
  /// exactly `::<token>::` is seen.
  stop_token: Option<String>,
  group_depth: u32,
  pending: Vec<RunnerEvent>,
  /// Only the `set-output` values applied during *this* run, in the order
  /// emitted. The full per-step output map (which also carries prior-stage
  /// and `$GITHUB_OUTPUT` outputs) lives on `ctx`; the caller merges this
  /// run-local map so last-writer-wins ordering is respected and stale
  /// outputs from a reused step id do not leak.
  set_outputs: Vec<(String, String)>,
}

impl CommandDispatcher {
  /// Create a dispatcher bound to a step id and the shared secret masker.
  ///
  /// Pass `ctx.masker().clone()` so masks registered here propagate to the
  /// file sink and every other reader of the shared masker.
  pub fn new(step_id: &str, masker: Arc<Mutex<SecretMasker>>) -> Self {
    Self {
      masker,
      step_id: step_id.to_owned(),
      echo_on: false,
      stop_token: None,
      group_depth: 0,
      pending: Vec::new(),
      set_outputs: Vec::new(),
    }
  }

  /// Inspect one stdout line, apply any workflow command, and report whether
  /// the caller should log the line.
  ///
  /// Command lines are parsed (via `command_parser::parse_command`), their
  /// props and data `%XX`-unescaped, then applied to `ctx`. Plain lines and
  /// lines seen while `stop-commands` is active return `PassThrough`.
  pub fn on_stdout_line(&mut self, line: &str, ctx: &mut ExecutionContext) -> LineDisposition {
    // While suspended, only the exact resume token re-enables processing.
    if let Some(token) = self.stop_token.clone() {
      if is_resume_marker(line, &token) {
        self.stop_token = None;
        return LineDisposition::Consumed;
      }
      return LineDisposition::PassThrough(line.to_owned());
    }

    let Some(command) = parse_command(line) else {
      return LineDisposition::PassThrough(line.to_owned());
    };

    if self.echo_on {
      // Echoed command lines reach the same event stream as plain output, so
      // a secret printed inside a `::<cmd>::` line must be masked here too —
      // the live-log / Results forwarder does not mask `Log` events it did
      // not produce verbatim.
      let masked = self.mask(line);
      self.pending.push(self.log_event(masked));
    }

    self.apply(command, ctx);
    LineDisposition::Consumed
  }

  /// Mask a line through the shared masker, recovering from a poisoned lock.
  fn mask(&self, line: &str) -> String {
    match self.masker.lock() {
      Ok(g) => g.mask(line),
      Err(poisoned) => poisoned.into_inner().mask(line),
    }
  }

  /// The `set-output` values applied during this run, in emission order.
  /// The caller merges these so last-writer-wins ordering holds and stale
  /// outputs on a reused step id do not leak.
  fn take_set_outputs(&mut self) -> Vec<(String, String)> {
    std::mem::take(&mut self.set_outputs)
  }

  /// Drain events (log groups, annotations, echoed commands) queued while
  /// processing lines. The caller forwards these on the async event channel.
  pub fn take_events(&mut self) -> Vec<RunnerEvent> {
    std::mem::take(&mut self.pending)
  }

  fn apply(&mut self, command: WorkflowCommand, ctx: &mut ExecutionContext) {
    match command {
      WorkflowCommand::SetOutput { name, value } => self.apply_set_output(&name, &value, ctx),
      WorkflowCommand::SaveState { name, value } => self.apply_save_state(&name, &value, ctx),
      WorkflowCommand::AddMask { value } => self.apply_add_mask(&value),
      WorkflowCommand::AddPath { value } => ctx.prepend_path(&unescape_data(&value)),
      WorkflowCommand::SetEnv { name, value } => self.apply_set_env(&name, &value, ctx),
      WorkflowCommand::Group { title } => self.apply_group(&title),
      WorkflowCommand::EndGroup => self.apply_endgroup(),
      WorkflowCommand::Error {
        message,
        file,
        line,
        ..
      } => self.push_annotation(AnnotationLevel::Error, &message, file, line),
      WorkflowCommand::Warning {
        message,
        file,
        line,
        ..
      } => self.push_annotation(AnnotationLevel::Warning, &message, file, line),
      WorkflowCommand::Notice {
        message,
        file,
        line,
        ..
      } => self.push_annotation(AnnotationLevel::Notice, &message, file, line),
      WorkflowCommand::Debug { message } => {
        tracing::debug!(
          step_id = self.step_id.as_str(),
          "{}",
          unescape_data(&message)
        );
      },
      WorkflowCommand::Echo { on } => self.echo_on = on,
      WorkflowCommand::StopCommands { token } => {
        // The upstream runner ignores an empty/zero-length stop token: a
        // stray `::stop-commands::` must not suspend command + mask
        // processing for the rest of the step.
        if !token.is_empty() {
          self.stop_token = Some(token);
        }
      },
      // `::<token>::` only matters while suspended; if processing is live it
      // is treated as a no-op command (matches the C# runner: a stray resume
      // marker outside a stop block is ignored).
      WorkflowCommand::ResumeCommands { .. } => {},
    }
  }

  fn apply_set_output(&mut self, name: &str, value: &str, ctx: &mut ExecutionContext) {
    if !name.is_empty() {
      let name = unescape_data(name);
      let value = unescape_data(value);
      ctx.set_step_output(&self.step_id, &name, &value);
      // Record only this run's set-outputs (emission order) so the caller
      // can merge with last-writer-wins semantics; `ctx.step_outputs` would
      // also surface prior-stage and file-command outputs.
      self.set_outputs.push((name, value));
    }
  }

  fn apply_save_state(&self, name: &str, value: &str, ctx: &mut ExecutionContext) {
    if !name.is_empty() {
      ctx.set_step_state(&self.step_id, &unescape_data(name), &unescape_data(value));
    }
  }

  fn apply_set_env(&self, name: &str, value: &str, ctx: &mut ExecutionContext) {
    if !name.is_empty() {
      ctx.set_env(&unescape_data(name), &unescape_data(value));
    }
  }

  fn apply_add_mask(&self, value: &str) {
    let secret = unescape_data(value);
    let mut guard = match self.masker.lock() {
      Ok(g) => g,
      Err(poisoned) => poisoned.into_inner(),
    };
    guard.add_secret(&secret);
  }

  fn apply_group(&mut self, title: &str) {
    self.group_depth = self.group_depth.saturating_add(1);
    let title = self.mask(&unescape_data(title));
    self.pending.push(RunnerEvent::LogGroup {
      step_id: self.step_id.clone(),
      title,
      open: true,
    });
  }

  fn apply_endgroup(&mut self) {
    self.group_depth = self.group_depth.saturating_sub(1);
    self.pending.push(RunnerEvent::LogGroup {
      step_id: self.step_id.clone(),
      title: String::new(),
      open: false,
    });
  }

  fn push_annotation(
    &mut self,
    level: AnnotationLevel,
    message: &str,
    file: Option<String>,
    line: Option<u32>,
  ) {
    // Annotation messages are surfaced in the Results UI / live-log, which the
    // forwarder does not mask; a secret printed inside `::error::`/`::warning::`/
    // `::notice::` must be redacted here at the single producer chokepoint.
    let message = self.mask(&unescape_data(message));
    self.pending.push(RunnerEvent::Annotation {
      step_id: self.step_id.clone(),
      level,
      message,
      file: file.map(|f| unescape_property(&f)),
      line,
    });
  }

  fn log_event(&self, line: String) -> RunnerEvent {
    RunnerEvent::Log {
      step_id: self.step_id.clone(),
      line,
      stream: shared::LogStream::Stdout,
    }
  }
}

/// A line is the resume marker for `token` if, ignoring surrounding
/// whitespace, it is exactly `::<token>::`.
fn is_resume_marker(line: &str, token: &str) -> bool {
  let trimmed = line.trim();
  let Some(inner) = trimmed.strip_prefix("::") else {
    return false;
  };
  let Some(inner) = inner.strip_suffix("::") else {
    return false;
  };
  inner == token
}

/// Unescape command **data** per the GitHub Actions workflow-command rules.
///
/// Matches the upstream runner's `UnescapeData`: decodes only `%25` → `%`,
/// `%0D` → `\r`, `%0A` → `\n` (case-insensitive hex). `%3A`/`%2C` are
/// *property-only* escapes and are left verbatim in data. Unrecognized `%XX`
/// sequences pass through unchanged.
fn unescape_data(value: &str) -> String {
  unescape_with(value, decode_data_escape)
}

/// Unescape command **property** values per `UnescapeProperty`: the data set
/// plus `%3A` → `:` and `%2C` → `,` (a property value is delimited by `:`/`,`,
/// so those are escaped on the wire and must be restored here).
fn unescape_property(value: &str) -> String {
  unescape_with(value, decode_property_escape)
}

/// Scan `value`, replacing each recognized `%XX` (by `decode`) and copying
/// every other char verbatim. Shared by data and property unescaping.
fn unescape_with(value: &str, decode: fn(&str) -> Option<char>) -> String {
  if !value.contains('%') {
    return value.to_owned();
  }
  let mut out = String::with_capacity(value.len());
  let bytes = value.as_bytes();
  let mut i = 0;
  while i < value.len() {
    if bytes.get(i) == Some(&b'%')
      && let Some(slice) = value.get(i + 1..i + 3)
      && let Some(replacement) = decode(slice)
    {
      out.push(replacement);
      i += 3;
      continue;
    }
    // Not a recognized escape: copy the next char verbatim.
    if let Some(ch) = value.get(i..).and_then(|s| s.chars().next()) {
      out.push(ch);
      i += ch.len_utf8();
    } else {
      break;
    }
  }
  out
}

/// Decode a data-only `%XX` escape (`%25`/`%0D`/`%0A`).
fn decode_data_escape(slice: &str) -> Option<char> {
  match slice.to_ascii_uppercase().as_str() {
    "25" => Some('%'),
    "0D" => Some('\r'),
    "0A" => Some('\n'),
    _ => None,
  }
}

/// Decode a property `%XX` escape: the data set plus `%3A`/`%2C`.
fn decode_property_escape(slice: &str) -> Option<char> {
  match slice.to_ascii_uppercase().as_str() {
    "3A" => Some(':'),
    "2C" => Some(','),
    _ => decode_data_escape(slice),
  }
}

/// Stream a step's stdout through the dispatcher as the child runs, emitting
/// each surviving line as a `Log` event immediately for realtime UI/blob.
///
/// Raw lines arrive on `stdout_rx` as the handler reads them off the child, so
/// a passthrough line is logged before the next is produced. Command lines are
/// applied to `ctx` and consumed; `group`/annotation commands emit their
/// events; other lines are masked (an `add-mask` applies before later lines)
/// and re-emitted. Returns the `set-output` map for `StepCompleted`.
pub async fn stream_dispatch_stdout(
  step_id: &str,
  stdout_rx: &mut mpsc::Receiver<String>,
  ctx: &mut ExecutionContext,
  events: &mpsc::Sender<RunnerEvent>,
) -> HashMap<String, String> {
  let mut dispatcher = CommandDispatcher::new(step_id, Arc::clone(ctx.masker()));
  while let Some(line) = stdout_rx.recv().await {
    let disposition = dispatcher.on_stdout_line(&line, ctx);
    for event in dispatcher.take_events() {
      let _ = events.send(event).await;
    }
    if let LineDisposition::PassThrough(text) = disposition {
      let masked = mask_line(ctx, &text);
      let _ = events
        .send(RunnerEvent::Log {
          step_id: step_id.to_owned(),
          line: masked,
          stream: shared::LogStream::Stdout,
        })
        .await;
    }
  }
  // Return only the `set-output` values this run produced (emission order →
  // last-writer-wins on merge), not `ctx.step_outputs` (which also carries
  // prior-stage + `$GITHUB_OUTPUT` outputs).
  dispatcher.take_set_outputs().into_iter().collect()
}

/// Mask a line through the shared masker, recovering from a poisoned lock.
fn mask_line(ctx: &ExecutionContext, line: &str) -> String {
  let guard = match ctx.masker().lock() {
    Ok(g) => g,
    Err(poisoned) => poisoned.into_inner(),
  };
  guard.mask(line)
}
