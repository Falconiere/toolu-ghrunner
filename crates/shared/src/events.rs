use std::collections::HashMap;

/// Conclusion of a step or job execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Conclusion {
  /// The step or job completed without error.
  Success,
  /// The step or job completed with an error.
  Failure,
  /// The step or job was cancelled before completing.
  Cancelled,
  /// The step or job was skipped (e.g. an `if:` condition was false).
  Skipped,
}

impl Conclusion {
  /// Convert to the GitHub protocol integer value for reporting.
  ///
  /// Values: Success=2, Failure=3, Cancelled=4, Skipped=7
  pub fn to_report_value(self) -> u8 {
    match self {
      Self::Success => 2,
      Self::Failure => 3,
      Self::Cancelled => 4,
      Self::Skipped => 7,
    }
  }

  /// Convert to the GitHub protocol string for reporting.
  pub fn to_report_string(self) -> &'static str {
    match self {
      Self::Success => "success",
      Self::Failure => "failure",
      Self::Cancelled => "cancelled",
      Self::Skipped => "skipped",
    }
  }
}

/// Which output stream a log line came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogStream {
  /// The line came from the step process's standard output.
  Stdout,
  /// The line came from the step process's standard error.
  Stderr,
}

/// Severity level for workflow annotations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnotationLevel {
  /// An informational annotation.
  Notice,
  /// A warning-level annotation.
  Warning,
  /// An error-level annotation.
  Error,
}

/// Events emitted during job execution via `Runner::execute_job()`.
#[derive(Debug, Clone)]
pub enum RunnerEvent {
  /// The job has started executing.
  JobStarted {
    /// The GitHub Actions job id.
    job_id: String,
    /// The human-readable job name.
    job_name: String,
  },
  /// A step has started executing.
  StepStarted {
    /// The step's id within the job.
    step_id: String,
    /// The human-readable step name.
    step_name: String,
    /// The step's 1-based position in the job's step sequence.
    step_number: u32,
  },
  /// A step finished executing.
  StepCompleted {
    /// The step's id within the job.
    step_id: String,
    /// The step's outcome.
    conclusion: Conclusion,
    /// The step's `outputs.<name>` values, keyed by output name.
    outputs: HashMap<String, String>,
  },
  /// A step was skipped without running.
  StepSkipped {
    /// The step's id within the job.
    step_id: String,
    /// Why the step was skipped.
    reason: String,
  },
  /// A log line produced by a running step. Unmasked — every consumer must
  /// mask it before writing to a durable sink.
  Log {
    /// The step's id within the job.
    step_id: String,
    /// The raw (unmasked) log line.
    line: String,
    /// Which stream the line came from.
    stream: LogStream,
  },
  /// A workflow-command log group boundary (`::group::` / `::endgroup::`).
  LogGroup {
    /// The step's id within the job.
    step_id: String,
    /// The group's title.
    title: String,
    /// `true` opens the group, `false` closes it.
    open: bool,
  },
  /// A workflow-command annotation (`::notice::` / `::warning::` / `::error::`).
  Annotation {
    /// The step's id within the job.
    step_id: String,
    /// The annotation's severity.
    level: AnnotationLevel,
    /// The annotation's message text.
    message: String,
    /// The file the annotation refers to, if any.
    file: Option<String>,
    /// The line number in `file` the annotation refers to, if any.
    line: Option<u32>,
  },
  /// The job has finished executing.
  JobCompleted {
    /// The GitHub Actions job id.
    job_id: String,
    /// The job's overall outcome.
    conclusion: Conclusion,
    /// The job's `outputs.<name>` values, keyed by output name.
    outputs: HashMap<String, String>,
  },
}

/// Events emitted by the `GitHubListener` — wraps `RunnerEvent` plus protocol events.
#[derive(Debug, Clone)]
pub enum ListenerEvent {
  /// A `RunnerEvent` from the execution engine.
  Runner(RunnerEvent),
  /// Broker session created.
  SessionCreated {
    /// The broker session id.
    session_id: String,
  },
  /// Job acquired from run service.
  JobAcquired {
    /// The GitHub Actions job id.
    job_id: String,
    /// The Run Service URL the job was acquired from.
    run_service_url: String,
  },
  /// Job lock renewed.
  LockRenewed {
    /// The new lock expiry timestamp, as reported by the broker.
    locked_until: String,
  },
  /// Step status reported to GitHub.
  ReportedStatus {
    /// The step's id within the job.
    step_id: String,
    /// The status string reported to GitHub.
    status: String,
  },
}
