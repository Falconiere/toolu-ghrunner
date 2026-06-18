use shared::{AnnotationLevel, Conclusion, ListenerEvent, LogStream, RunnerEvent};
use std::collections::HashMap;

#[test]
fn conclusion_report_values() {
  assert_eq!(Conclusion::Success.to_report_value(), 2);
  assert_eq!(Conclusion::Failure.to_report_value(), 3);
  assert_eq!(Conclusion::Cancelled.to_report_value(), 4);
  assert_eq!(Conclusion::Skipped.to_report_value(), 7);
}

#[test]
fn conclusion_report_strings() {
  assert_eq!(Conclusion::Success.to_report_string(), "success");
  assert_eq!(Conclusion::Failure.to_report_string(), "failure");
  assert_eq!(Conclusion::Cancelled.to_report_string(), "cancelled");
  assert_eq!(Conclusion::Skipped.to_report_string(), "skipped");
}

#[test]
fn runner_event_job_started() {
  let ev = RunnerEvent::JobStarted {
    job_id: "job-1".to_owned(),
    job_name: "build".to_owned(),
  };
  match ev {
    RunnerEvent::JobStarted { job_id, job_name } => {
      assert_eq!(job_id, "job-1");
      assert_eq!(job_name, "build");
    },
    RunnerEvent::StepStarted { .. }
    | RunnerEvent::StepCompleted { .. }
    | RunnerEvent::StepSkipped { .. }
    | RunnerEvent::Log { .. }
    | RunnerEvent::LogGroup { .. }
    | RunnerEvent::Annotation { .. }
    | RunnerEvent::JobCompleted { .. } => {
      // Only JobStarted is constructed above; other arms are unreachable.
    },
  }
}

#[test]
fn runner_event_step_completed_with_outputs() {
  let mut outputs = HashMap::new();
  outputs.insert("sha".to_owned(), "abc123".to_owned());
  let ev = RunnerEvent::StepCompleted {
    step_id: "step-1".to_owned(),
    conclusion: Conclusion::Success,
    outputs,
  };
  if let RunnerEvent::StepCompleted {
    step_id,
    conclusion,
    outputs,
  } = ev
  {
    assert_eq!(step_id, "step-1");
    assert_eq!(conclusion, Conclusion::Success);
    assert_eq!(outputs.get("sha").map(String::as_str), Some("abc123"));
  }
}

#[test]
fn listener_event_wraps_runner() {
  let ev = ListenerEvent::Runner(RunnerEvent::JobStarted {
    job_id: "j".to_owned(),
    job_name: "n".to_owned(),
  });
  match ev {
    ListenerEvent::Runner(RunnerEvent::JobStarted { job_id, job_name }) => {
      assert_eq!(job_id, "j");
      assert_eq!(job_name, "n");
    },
    ListenerEvent::Runner(_)
    | ListenerEvent::SessionCreated { .. }
    | ListenerEvent::JobAcquired { .. }
    | ListenerEvent::LockRenewed { .. }
    | ListenerEvent::ReportedStatus { .. } => {},
  }
}

#[test]
fn annotation_level_eq() {
  assert_eq!(AnnotationLevel::Notice, AnnotationLevel::Notice);
  assert_ne!(AnnotationLevel::Notice, AnnotationLevel::Error);
}

#[test]
fn log_stream_eq() {
  assert_eq!(LogStream::Stdout, LogStream::Stdout);
  assert_ne!(LogStream::Stdout, LogStream::Stderr);
}
