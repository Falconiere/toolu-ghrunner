//! AC-4: stdout workflow-command dispatch on the live step path.
//!
//! Drives real `bash` script steps through the real step loop
//! (`execution::steps_runner::run_steps`) — no mocks, no stubs — and asserts
//! that `::set-output::`, `$GITHUB_OUTPUT`, `::add-mask::`, `::error::`, `%XX`
//! unescaping, and `::stop-commands::` behave like the upstream GitHub
//! Actions runner. The committed `job_message.json` fixture is loaded to
//! confirm it deserializes into the same `AgentJobRequestMessage` shape the
//! engine consumes; the steps under test are driven through that same engine.

use std::collections::HashMap;
use std::error::Error;
use std::sync::{Arc, Mutex};

use shared::{
  ActionStep, AgentJobRequestMessage, AnnotationLevel, Conclusion, LogStream, RunnerConfig,
  RunnerEvent,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use toolu_runner::execution::context::ExecutionContext;
use toolu_runner::execution::secret_masker::SecretMasker;
use toolu_runner::execution::steps_runner::run_steps;

const JOB_MESSAGE: &str = include_str!("fixtures/job_message.json");

type TestResult<T> = Result<T, Box<dyn Error>>;

/// Confirm the committed fixture deserializes into the engine's message type;
/// the per-test steps are then driven through that same engine.
fn fixture_job() -> TestResult<AgentJobRequestMessage> {
  Ok(serde_json::from_str(JOB_MESSAGE)?)
}

/// Drive `steps` through the real step loop and collect every emitted event.
///
/// Returns the events plus the shared masker (the instance the engine
/// registers `::add-mask::` values onto — same role as the tracing redactor).
async fn run_steps_collect(
  steps: Vec<ActionStep>,
) -> TestResult<(Vec<RunnerEvent>, Arc<Mutex<SecretMasker>>)> {
  // The fixture is real data; confirm it round-trips into the engine's type.
  if fixture_job()?.job_id.is_empty() {
    return Err("fixture job_id missing".into());
  }

  let dir = tempfile::tempdir()?;
  let workspace = dir.path().join("work");
  std::fs::create_dir_all(&workspace)?;
  let config = RunnerConfig {
    data_dir: dir.path().join("data"),
    workspace_root: dir.path().join("work"),
    cgroup_path: None,
    services_mode: shared::ServicesMode::default(),
  };
  std::fs::create_dir_all(&config.data_dir)?;

  let masker = Arc::new(Mutex::new(SecretMasker::new()));
  let mut ctx = ExecutionContext::with_masker(Arc::clone(&masker));

  let (tx, mut rx) = mpsc::channel::<RunnerEvent>(1024);
  let collector = tokio::spawn(async move {
    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
      events.push(event);
    }
    events
  });

  let spec = toolu_runner::execution::job_spec::JobSpec::default();
  run_steps(
    &steps,
    &mut ctx,
    &tx,
    CancellationToken::new(),
    &toolu_runner::execution::steps_runner::JobRun {
      workspace: &workspace,
      config: &config,
      spec: &spec,
    },
  )
  .await?;

  drop(tx);
  let events = collector.await?;
  drop(dir);
  Ok((events, masker))
}

/// Find the `StepCompleted` outputs for a step id.
fn step_outputs(events: &[RunnerEvent], step_id: &str) -> HashMap<String, String> {
  for event in events {
    if let RunnerEvent::StepCompleted {
      step_id: id,
      outputs,
      ..
    } = event
      && id == step_id
    {
      return outputs.clone();
    }
  }
  HashMap::new()
}

/// Collect all stdout log lines for a step id.
fn step_log_lines(events: &[RunnerEvent], step_id: &str) -> Vec<String> {
  let mut lines = Vec::new();
  for event in events {
    if let RunnerEvent::Log {
      step_id: id,
      line,
      stream: LogStream::Stdout,
    } = event
      && id == step_id
    {
      lines.push(line.clone());
    }
  }
  lines
}

/// Find the first `Annotation` event for a step id.
fn step_annotation(
  events: &[RunnerEvent],
  step_id: &str,
) -> Option<(AnnotationLevel, String, Option<String>, Option<u32>)> {
  for event in events {
    if let RunnerEvent::Annotation {
      step_id: id,
      level,
      message,
      file,
      line,
    } = event
      && id == step_id
    {
      return Some((*level, message.clone(), file.clone(), *line));
    }
  }
  None
}

#[tokio::test]
async fn set_output_stdout_command_sets_step_output() -> TestResult<()> {
  let step = ActionStep::script("s1", "echo \"::set-output name=x::v\"", "");
  let (events, _masker) = run_steps_collect(vec![step]).await?;
  let outputs = step_outputs(&events, "s1");
  assert_eq!(
    outputs.get("x").map(String::as_str),
    Some("v"),
    "::set-output:: should set step output x=v; outputs={outputs:?}"
  );
  Ok(())
}

#[tokio::test]
async fn github_output_file_sets_step_output() -> TestResult<()> {
  let step = ActionStep::script("s2", "echo \"x=v\" >> \"$GITHUB_OUTPUT\"", "");
  let (events, _masker) = run_steps_collect(vec![step]).await?;
  let outputs = step_outputs(&events, "s2");
  assert_eq!(
    outputs.get("x").map(String::as_str),
    Some("v"),
    "$GITHUB_OUTPUT should set step output x=v; outputs={outputs:?}"
  );
  Ok(())
}

#[tokio::test]
async fn add_mask_redacts_later_lines_and_shared_masker() -> TestResult<()> {
  let script = "echo \"::add-mask::s3cretValue\"\necho \"leaked s3cretValue here\"";
  let step = ActionStep::script("s3", script, "");
  let (events, masker) = run_steps_collect(vec![step]).await?;

  // The output line emitted AFTER ::add-mask:: must be masked. (The
  // `##[group]Run …` header echoes the script source verbatim, which predates
  // the runtime add-mask — matching the upstream runner — so it is excluded.)
  let lines = step_log_lines(&events, "s3");
  let output_line = lines
    .iter()
    .find(|l| l.contains("leaked") && !l.starts_with("##["))
    .ok_or("the echoed output line was not emitted")?;
  assert!(
    !output_line.contains("s3cretValue"),
    "secret must be masked in output emitted after ::add-mask::; line={output_line:?}"
  );
  assert_eq!(
    output_line, "leaked *** here",
    "secret should be replaced with ***; line={output_line:?}"
  );

  // The SHARED masker (also the tracing file-sink redactor) learned the secret.
  let guard = masker.lock().expect("masker lock");
  assert_eq!(
    guard.mask("prefix s3cretValue suffix"),
    "prefix *** suffix",
    "::add-mask:: must register on the shared masker instance"
  );
  Ok(())
}

#[tokio::test]
async fn error_command_emits_annotation_with_file_and_line() -> TestResult<()> {
  let step = ActionStep::script("s4", "echo \"::error file=a.js,line=2::boom\"", "");
  let (events, _masker) = run_steps_collect(vec![step]).await?;

  let (level, message, file, line) =
    step_annotation(&events, "s4").ok_or("no Annotation event for step s4")?;
  assert_eq!(level, AnnotationLevel::Error);
  assert_eq!(message, "boom");
  assert_eq!(file.as_deref(), Some("a.js"));
  assert_eq!(line, Some(2));
  Ok(())
}

#[tokio::test]
async fn percent_escapes_are_decoded_in_output_value() -> TestResult<()> {
  // A set-output value is command DATA: the upstream runner's `UnescapeData`
  // decodes only `%25`/`%0D`/`%0A`. `%3A`/`%2C` are property-only escapes and
  // are left verbatim in data (they would be decoded in a command property
  // like `file=`/`line=`, not in the message/value).
  let step = ActionStep::script("s5", "echo \"::set-output name=k::a%0Ab%3Ac\"", "");
  let (events, _masker) = run_steps_collect(vec![step]).await?;
  let outputs = step_outputs(&events, "s5");
  assert_eq!(
    outputs.get("k").map(String::as_str),
    Some("a\nb%3Ac"),
    "%0A -> newline in data; %3A stays verbatim (property-only); outputs={outputs:?}"
  );
  Ok(())
}

#[tokio::test]
async fn stop_commands_suspends_inner_command_processing() -> TestResult<()> {
  // Between ::stop-commands::TOKEN and ::TOKEN:: the inner ::set-output:: is
  // treated as plain text, so output `inner` must NOT be set. The output
  // after the resume marker IS processed.
  let script = "\
echo \"::stop-commands::MYTOKEN\"
echo \"::set-output name=inner::should_not_set\"
echo \"::MYTOKEN::\"
echo \"::set-output name=after::set_ok\"";
  let step = ActionStep::script("s6", script, "");
  let (events, _masker) = run_steps_collect(vec![step]).await?;
  let outputs = step_outputs(&events, "s6");

  assert!(
    !outputs.contains_key("inner"),
    "command inside stop-commands block must not be processed; outputs={outputs:?}"
  );
  assert_eq!(
    outputs.get("after").map(String::as_str),
    Some("set_ok"),
    "command after the resume marker must be processed; outputs={outputs:?}"
  );
  Ok(())
}

#[tokio::test]
async fn non_command_lines_pass_through_verbatim() -> TestResult<()> {
  let step = ActionStep::script("s7", "echo \"hello world\"", "");
  let (events, _masker) = run_steps_collect(vec![step]).await?;
  let lines = step_log_lines(&events, "s7");
  assert!(
    lines.iter().any(|l| l == "hello world"),
    "plain stdout must be logged verbatim; lines={lines:?}"
  );
  Ok(())
}

#[tokio::test]
async fn malformed_command_line_is_logged_not_dropped() -> TestResult<()> {
  // Starts with `::` but is not a valid command — must pass through verbatim.
  let step = ActionStep::script("s8", "echo \"::not-a-real-command::\"", "");
  let (events, _masker) = run_steps_collect(vec![step]).await?;
  let lines = step_log_lines(&events, "s8");
  assert!(
    lines.iter().any(|l| l == "::not-a-real-command::"),
    "unparsable ::command:: must be logged verbatim; lines={lines:?}"
  );
  Ok(())
}

#[tokio::test]
async fn save_state_does_not_leak_into_outputs() -> TestResult<()> {
  let step = ActionStep::script("s9", "echo \"::save-state name=k::v\"", "");
  let (events, _masker) = run_steps_collect(vec![step]).await?;
  let outputs = step_outputs(&events, "s9");
  assert!(
    !outputs.contains_key("k"),
    "save-state writes step state, not outputs; outputs={outputs:?}"
  );
  // Step still completes successfully.
  let completed = events.iter().any(|e| {
    matches!(
      e,
      RunnerEvent::StepCompleted { step_id, conclusion, .. }
        if step_id == "s9" && *conclusion == Conclusion::Success
    )
  });
  assert!(completed, "step s9 should complete successfully");
  Ok(())
}
