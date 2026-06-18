use std::collections::HashMap;

/// Conclusion of a step or job execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Conclusion {
  Success,
  Failure,
  Cancelled,
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
  Stdout,
  Stderr,
}

/// Severity level for workflow annotations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnotationLevel {
  Notice,
  Warning,
  Error,
}

/// Events emitted during job execution via `Runner::execute_job()`.
#[derive(Debug, Clone)]
pub enum RunnerEvent {
  JobStarted {
    job_id: String,
    job_name: String,
  },
  StepStarted {
    step_id: String,
    step_name: String,
    step_number: u32,
  },
  StepCompleted {
    step_id: String,
    conclusion: Conclusion,
    outputs: HashMap<String, String>,
  },
  StepSkipped {
    step_id: String,
    reason: String,
  },
  Log {
    step_id: String,
    line: String,
    stream: LogStream,
  },
  LogGroup {
    step_id: String,
    title: String,
    open: bool,
  },
  Annotation {
    step_id: String,
    level: AnnotationLevel,
    message: String,
    file: Option<String>,
    line: Option<u32>,
  },
  JobCompleted {
    job_id: String,
    conclusion: Conclusion,
    outputs: HashMap<String, String>,
  },
}

/// Events emitted by the `GitHubListener` — wraps `RunnerEvent` plus protocol events.
#[derive(Debug, Clone)]
pub enum ListenerEvent {
  /// A `RunnerEvent` from the execution engine.
  Runner(RunnerEvent),
  /// Broker session created.
  SessionCreated { session_id: String },
  /// Job acquired from run service.
  JobAcquired {
    job_id: String,
    run_service_url: String,
  },
  /// Job lock renewed.
  LockRenewed { locked_until: String },
  /// Step status reported to GitHub.
  ReportedStatus { step_id: String, status: String },
}
