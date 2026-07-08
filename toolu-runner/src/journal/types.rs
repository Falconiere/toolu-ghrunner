//! On-disk journal contract (v1): serde types for one JSONL line per
//! `ListenerEvent`, deliberately decoupled from the in-memory enums in
//! `shared::events` so the file format can hold shape while internals evolve.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use shared::{AnnotationLevel, Conclusion, ListenerEvent, LogStream, RunnerEvent};

/// Journal contract version written by this build.
pub const JOURNAL_VERSION: u32 = 1;

/// One journal line: version/sequence/timestamp envelope + flattened event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JournalLine {
  /// Contract version; readers skip lines whose `v` they don't know.
  pub v: u32,
  /// Monotonic per-file sequence, starting at 0.
  pub seq: u64,
  /// RFC3339 UTC timestamp stamped by the writer.
  pub ts: String,
  /// Event payload, flattened into the same JSON object under a `type` tag.
  #[serde(flatten)]
  pub event: JournalEvent,
}

/// Journal event payload — the on-disk mirror of `ListenerEvent`.
///
/// Enum-typed fields are serialized as lowercase strings (`conclusion`,
/// `stream`, `level`) so the file is self-describing without this crate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum JournalEvent {
  SessionCreated {
    session_id: String,
  },
  JobAcquired {
    job_id: String,
    run_service_url: String,
  },
  JobStarted {
    job_id: String,
    job_name: String,
  },
  StepStarted {
    step_id: String,
    step_name: String,
    step_number: u32,
  },
  Log {
    step_id: String,
    line: String,
    /// `"stdout"` or `"stderr"`.
    stream: String,
  },
  LogGroup {
    step_id: String,
    title: String,
    open: bool,
  },
  Annotation {
    step_id: String,
    /// `"notice"`, `"warning"`, or `"error"`.
    level: String,
    message: String,
    file: Option<String>,
    line: Option<u32>,
  },
  StepCompleted {
    step_id: String,
    /// `"success"`, `"failure"`, `"cancelled"`, or `"skipped"`.
    conclusion: String,
    outputs: HashMap<String, String>,
  },
  StepSkipped {
    step_id: String,
    reason: String,
  },
  JobCompleted {
    job_id: String,
    conclusion: String,
    outputs: HashMap<String, String>,
  },
  LockRenewed {
    locked_until: String,
  },
  ReportedStatus {
    step_id: String,
    status: String,
  },
}

impl From<&ListenerEvent> for JournalEvent {
  fn from(ev: &ListenerEvent) -> Self {
    match ev {
      ListenerEvent::Runner(r) => Self::from(r),
      ListenerEvent::SessionCreated { session_id } => JournalEvent::SessionCreated {
        session_id: session_id.clone(),
      },
      ListenerEvent::JobAcquired {
        job_id,
        run_service_url,
      } => JournalEvent::JobAcquired {
        job_id: job_id.clone(),
        run_service_url: run_service_url.clone(),
      },
      ListenerEvent::LockRenewed { locked_until } => JournalEvent::LockRenewed {
        locked_until: locked_until.clone(),
      },
      ListenerEvent::ReportedStatus { step_id, status } => JournalEvent::ReportedStatus {
        step_id: step_id.clone(),
        status: status.clone(),
      },
    }
  }
}

impl From<&RunnerEvent> for JournalEvent {
  fn from(ev: &RunnerEvent) -> Self {
    use RunnerEvent as R;
    match ev {
      R::JobStarted { job_id, job_name } => job_started(job_id, job_name),
      R::StepStarted {
        step_id,
        step_name,
        step_number,
      } => step_started(step_id, step_name, *step_number),
      R::Log {
        step_id,
        line,
        stream,
      } => log(step_id, line, *stream),
      R::LogGroup {
        step_id,
        title,
        open,
      } => log_group(step_id, title, *open),
      R::Annotation {
        step_id,
        level,
        message,
        file,
        line,
      } => annotation(step_id, *level, message, file.as_deref(), *line),
      R::StepCompleted {
        step_id,
        conclusion,
        outputs,
      } => step_completed(step_id, *conclusion, outputs),
      R::StepSkipped { step_id, reason } => step_skipped(step_id, reason),
      R::JobCompleted {
        job_id,
        conclusion,
        outputs,
      } => job_completed(job_id, *conclusion, outputs),
    }
  }
}

fn job_started(job_id: &str, job_name: &str) -> JournalEvent {
  JournalEvent::JobStarted {
    job_id: job_id.to_owned(),
    job_name: job_name.to_owned(),
  }
}

fn step_started(step_id: &str, step_name: &str, step_number: u32) -> JournalEvent {
  JournalEvent::StepStarted {
    step_id: step_id.to_owned(),
    step_name: step_name.to_owned(),
    step_number,
  }
}

fn log(step_id: &str, line: &str, stream: LogStream) -> JournalEvent {
  JournalEvent::Log {
    step_id: step_id.to_owned(),
    line: line.to_owned(),
    stream: stream_str(stream).to_owned(),
  }
}

fn log_group(step_id: &str, title: &str, open: bool) -> JournalEvent {
  JournalEvent::LogGroup {
    step_id: step_id.to_owned(),
    title: title.to_owned(),
    open,
  }
}

fn annotation(
  step_id: &str,
  level: AnnotationLevel,
  message: &str,
  file: Option<&str>,
  line: Option<u32>,
) -> JournalEvent {
  JournalEvent::Annotation {
    step_id: step_id.to_owned(),
    level: level_str(level).to_owned(),
    message: message.to_owned(),
    file: file.map(str::to_owned),
    line,
  }
}

fn step_completed(
  step_id: &str,
  conclusion: Conclusion,
  outputs: &HashMap<String, String>,
) -> JournalEvent {
  JournalEvent::StepCompleted {
    step_id: step_id.to_owned(),
    conclusion: conclusion.to_report_string().to_owned(),
    outputs: outputs.clone(),
  }
}

fn step_skipped(step_id: &str, reason: &str) -> JournalEvent {
  JournalEvent::StepSkipped {
    step_id: step_id.to_owned(),
    reason: reason.to_owned(),
  }
}

fn job_completed(
  job_id: &str,
  conclusion: Conclusion,
  outputs: &HashMap<String, String>,
) -> JournalEvent {
  JournalEvent::JobCompleted {
    job_id: job_id.to_owned(),
    conclusion: conclusion.to_report_string().to_owned(),
    outputs: outputs.clone(),
  }
}

/// Lowercase stream name for the journal (`stdout` / `stderr`).
fn stream_str(s: LogStream) -> &'static str {
  match s {
    LogStream::Stdout => "stdout",
    LogStream::Stderr => "stderr",
  }
}

/// Lowercase annotation level for the journal (`notice` / `warning` / `error`).
fn level_str(l: AnnotationLevel) -> &'static str {
  match l {
    AnnotationLevel::Notice => "notice",
    AnnotationLevel::Warning => "warning",
    AnnotationLevel::Error => "error",
  }
}

/// Journal conclusion string for a `Conclusion` (same values GitHub reports).
pub fn conclusion_str(c: Conclusion) -> &'static str {
  c.to_report_string()
}

/// Sanitize a job id for use in a journal file name: every char outside
/// `[A-Za-z0-9._-]` becomes one `_`; no collapsing, no truncation.
pub fn sanitize_job_id(job_id: &str) -> String {
  job_id
    .chars()
    .map(|c| {
      if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
        c
      } else {
        '_'
      }
    })
    .collect()
}
