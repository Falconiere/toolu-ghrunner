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
  /// The broker session was created; long-polling for jobs is starting.
  SessionCreated {
    /// The broker session id.
    session_id: String,
  },
  /// A job was acquired off the broker queue.
  JobAcquired {
    /// The acquired job's id.
    job_id: String,
    /// The Run Service URL used to report status for this job.
    run_service_url: String,
  },
  /// Job execution began.
  JobStarted {
    /// The job's id.
    job_id: String,
    /// The job's display name.
    job_name: String,
  },
  /// A step began executing.
  StepStarted {
    /// The step's id.
    step_id: String,
    /// The step's display name.
    step_name: String,
    /// The step's 1-based position in the job.
    step_number: u32,
  },
  /// One line of step output.
  Log {
    /// The id of the step this log line belongs to.
    step_id: String,
    /// The log line's text.
    line: String,
    /// `"stdout"` or `"stderr"`.
    stream: String,
  },
  /// A log group was opened or closed.
  LogGroup {
    /// The id of the step this log group belongs to.
    step_id: String,
    /// The log group's title.
    title: String,
    /// `true` if the group is opening, `false` if it is closing.
    open: bool,
  },
  /// A notice/warning/error annotation was raised.
  Annotation {
    /// The id of the step this annotation belongs to.
    step_id: String,
    /// `"notice"`, `"warning"`, or `"error"`.
    level: String,
    /// The annotation's message text.
    message: String,
    /// The file path the annotation refers to, if any.
    file: Option<String>,
    /// The line number in `file` the annotation refers to, if any.
    line: Option<u32>,
  },
  /// A step finished.
  StepCompleted {
    /// The completed step's id.
    step_id: String,
    /// `"success"`, `"failure"`, `"cancelled"`, or `"skipped"`.
    conclusion: String,
    /// The step's declared outputs, by name.
    outputs: HashMap<String, String>,
  },
  /// A step was skipped (its `if:` condition was falsy).
  StepSkipped {
    /// The skipped step's id.
    step_id: String,
    /// Why the step was skipped.
    reason: String,
  },
  /// The job finished.
  JobCompleted {
    /// The completed job's id.
    job_id: String,
    /// `"success"`, `"failure"`, `"cancelled"`, or `"skipped"`.
    conclusion: String,
    /// The job's declared outputs, by name.
    outputs: HashMap<String, String>,
  },
  /// The job lock was renewed with the broker.
  LockRenewed {
    /// RFC3339 UTC timestamp of when the renewed lock expires.
    locked_until: String,
  },
  /// A step status was reported to the Results Service.
  ReportedStatus {
    /// The id of the step whose status was reported.
    step_id: String,
    /// The reported status string.
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
    conclusion: conclusion_str(conclusion).to_owned(),
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
    conclusion: conclusion_str(conclusion).to_owned(),
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
