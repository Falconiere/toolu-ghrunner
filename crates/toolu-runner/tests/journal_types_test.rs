//! Journal line contract (v1) — the spec's example line verbatim, a serde
//! round-trip per `JournalEvent` variant, enum-to-string mapping, and job-id
//! sanitization. AC-11 partial: fixture-drawn assertions live in
//! `journal_reader_test` once the canonical fixture is committed.

use std::collections::HashMap;
use std::error::Error;

use observability::journal::{JOURNAL_VERSION, JournalEvent, JournalLine};
use shared::paths::sanitize_job_id;
use shared::{AnnotationLevel, Conclusion, ListenerEvent, LogStream, RunnerEvent};

type TestResult = Result<(), Box<dyn Error>>;

/// The exact example line from the design spec (§ Journal line contract).
const SPEC_EXAMPLE: &str = r#"{"v":1,"seq":12,"ts":"2026-07-08T12:34:56.789Z","type":"log","step_id":"s1","line":"hello","stream":"stdout"}"#;

#[test]
fn spec_example_line_round_trips() -> TestResult {
  let line: JournalLine = serde_json::from_str(SPEC_EXAMPLE)?;
  assert_eq!(line.v, JOURNAL_VERSION);
  assert_eq!(line.seq, 12);
  assert_eq!(line.ts, "2026-07-08T12:34:56.789Z");
  assert_eq!(
    line.event,
    JournalEvent::Log {
      step_id: "s1".to_owned(),
      line: "hello".to_owned(),
      stream: "stdout".to_owned(),
    }
  );
  // Field-for-field: re-serialization equals the spec's JSON.
  let reserialized = serde_json::to_value(&line)?;
  let original: serde_json::Value = serde_json::from_str(SPEC_EXAMPLE)?;
  assert_eq!(reserialized, original);
  Ok(())
}

const JOB_ID: &str = "5d2bc9f6-4f7f-4a3b-8f2a-1c9d0e7a6b5c";

/// The engine-side (`Runner`-wrapped) events, one per `RunnerEvent` variant.
fn runner_events() -> Vec<RunnerEvent> {
  let outputs: HashMap<String, String> = [("digest".to_owned(), "sha256:abc123".to_owned())].into();
  vec![
    RunnerEvent::JobStarted {
      job_id: JOB_ID.to_owned(),
      job_name: "build".to_owned(),
    },
    RunnerEvent::StepStarted {
      step_id: "step-1".to_owned(),
      step_name: "Checkout".to_owned(),
      step_number: 1,
    },
    RunnerEvent::Log {
      step_id: "step-1".to_owned(),
      line: "hello \"quoted\" world".to_owned(),
      stream: LogStream::Stderr,
    },
    RunnerEvent::LogGroup {
      step_id: "step-1".to_owned(),
      title: "Run actions/checkout@v4".to_owned(),
      open: true,
    },
    RunnerEvent::Annotation {
      step_id: "step-1".to_owned(),
      level: AnnotationLevel::Warning,
      message: "deprecated input".to_owned(),
      file: Some(".github/workflows/ci.yml".to_owned()),
      line: Some(12),
    },
    RunnerEvent::StepCompleted {
      step_id: "step-1".to_owned(),
      conclusion: Conclusion::Success,
      outputs: outputs.clone(),
    },
    RunnerEvent::StepSkipped {
      step_id: "step-2".to_owned(),
      reason: "condition evaluated to false".to_owned(),
    },
    RunnerEvent::JobCompleted {
      job_id: JOB_ID.to_owned(),
      conclusion: Conclusion::Failure,
      outputs,
    },
  ]
}

/// One `ListenerEvent` per journal variant, with realistic field shapes.
fn all_listener_events() -> Vec<ListenerEvent> {
  let mut events = vec![
    ListenerEvent::SessionCreated {
      session_id: "00000000-0000-0000-0000-000000000000".to_owned(),
    },
    ListenerEvent::JobAcquired {
      job_id: JOB_ID.to_owned(),
      run_service_url: "https://run-actions-3-azure-eastus.actions.githubusercontent.com"
        .to_owned(),
    },
  ];
  events.extend(runner_events().into_iter().map(ListenerEvent::Runner));
  events.push(ListenerEvent::LockRenewed {
    locked_until: "2026-07-08T12:35:56Z".to_owned(),
  });
  events.push(ListenerEvent::ReportedStatus {
    step_id: "step-1".to_owned(),
    status: "completed".to_owned(),
  });
  events
}

#[test]
fn every_variant_round_trips() -> TestResult {
  let events = all_listener_events();
  assert_eq!(events.len(), 12, "one ListenerEvent per journal variant");
  for (i, ev) in events.iter().enumerate() {
    let line = JournalLine {
      v: JOURNAL_VERSION,
      seq: i as u64,
      ts: "2026-07-08T00:00:00.000Z".to_owned(),
      event: JournalEvent::from(ev),
    };
    let json = serde_json::to_string(&line)?;
    let back: JournalLine = serde_json::from_str(&json)?;
    assert_eq!(
      back, line,
      "variant {i} failed round-trip; wire form: {json}"
    );
    // Every line carries the envelope and a snake_case type tag.
    let value: serde_json::Value = serde_json::from_str(&json)?;
    let tag = value
      .get("type")
      .and_then(serde_json::Value::as_str)
      .unwrap_or_default();
    assert!(
      !tag.is_empty() && tag.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
      "variant {i} tag not snake_case: {json}"
    );
  }
  Ok(())
}

#[test]
fn conversion_maps_enums_to_lowercase_strings() {
  let ev = JournalEvent::from(&RunnerEvent::StepCompleted {
    step_id: "s".to_owned(),
    conclusion: Conclusion::Cancelled,
    outputs: HashMap::new(),
  });
  assert!(
    matches!(&ev, JournalEvent::StepCompleted { conclusion, .. } if conclusion == "cancelled"),
    "StepCompleted conversion produced {ev:?}"
  );

  let ev = JournalEvent::from(&RunnerEvent::Log {
    step_id: "s".to_owned(),
    line: "l".to_owned(),
    stream: LogStream::Stdout,
  });
  assert!(
    matches!(&ev, JournalEvent::Log { stream, .. } if stream == "stdout"),
    "Log conversion produced {ev:?}"
  );

  let ev = JournalEvent::from(&RunnerEvent::Annotation {
    step_id: "s".to_owned(),
    level: AnnotationLevel::Error,
    message: "m".to_owned(),
    file: None,
    line: None,
  });
  assert!(
    matches!(&ev, JournalEvent::Annotation { level, .. } if level == "error"),
    "Annotation conversion produced {ev:?}"
  );
}

#[test]
fn sanitize_replaces_disallowed_chars_one_to_one() {
  // Spec example: no collapsing, no truncation.
  assert_eq!(sanitize_job_id("job/id[x]"), "job_id_x_");
  // Allowed charset passes through untouched.
  assert_eq!(sanitize_job_id("abc-123_D.e"), "abc-123_D.e");
  // Consecutive disallowed chars stay one-to-one.
  assert_eq!(sanitize_job_id("a//b"), "a__b");
  // Non-ASCII chars each map to a single underscore.
  assert_eq!(sanitize_job_id("üé"), "__");
}
