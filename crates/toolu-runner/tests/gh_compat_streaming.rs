//! Realtime stdout streaming regression guard (S7 fix).
//!
//! S7 made run-step stdout captured/buffered so the command dispatcher could
//! parse `::commands::` and `$GITHUB_OUTPUT`; the side effect was that stdout
//! `Log` events (and thus the live-log WebSocket feed) only fired AFTER the
//! step finished. These tests drive real `bash` steps through the real step
//! loop (`execution::steps_runner::run_steps`) — no mocks — and assert that
//! passthrough stdout `Log` events are emitted INCREMENTALLY, line-by-line,
//! while the child is still running, WITHOUT regressing workflow-command
//! dispatch (`::set-output::` consumed, `::add-mask::` masks later lines).
//!
//! The committed `job_message.json` fixture is loaded to confirm it
//! deserializes into the same `AgentJobRequestMessage` shape the engine
//! consumes; the steps under test are driven through that same engine.

use std::error::Error;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use execution::execution::context::ExecutionContext;
use execution::execution::job_spec::JobSpec;
use execution::execution::steps_runner::{JobRun, run_steps};
use shared::SecretMasker;
use shared::{ActionStep, AgentJobRequestMessage, LogStream, RunnerConfig, RunnerEvent};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const JOB_MESSAGE: &str = include_str!("fixtures/job_message.json");

type TestResult<T> = Result<T, Box<dyn Error>>;

/// Confirm the committed fixture deserializes into the engine's message type.
fn fixture_job() -> TestResult<AgentJobRequestMessage> {
  Ok(serde_json::from_str(JOB_MESSAGE)?)
}

/// One timestamped event observed on the receiver as `run_steps` emits it.
struct Timed {
  at: Instant,
  event: RunnerEvent,
}

/// Build a throwaway `RunnerConfig` rooted under `dir` with its `work`/`data`
/// dirs created. The caller keeps `dir` alive for the run's duration.
fn test_config(dir: &std::path::Path) -> TestResult<(RunnerConfig, std::path::PathBuf)> {
  let workspace = dir.join("work");
  std::fs::create_dir_all(&workspace)?;
  let config = RunnerConfig {
    data_dir: dir.join("data"),
    workspace_root: workspace.clone(),
    cgroup_path: None,
    services_mode: shared::ServicesMode::default(),
    ..RunnerConfig::default()
  };
  std::fs::create_dir_all(&config.data_dir)?;
  Ok((config, workspace))
}

/// Drive `steps` through the real step loop, recording the arrival `Instant`
/// of every event so tests can assert ordering/timing. Returns the timed
/// events plus the shared masker (the `::add-mask::` registration target).
async fn run_steps_timed(
  steps: Vec<ActionStep>,
) -> TestResult<(Vec<Timed>, Arc<Mutex<SecretMasker>>)> {
  // The fixture is real data; confirm it round-trips into the engine's type.
  if fixture_job()?.job_id.is_empty() {
    return Err("fixture job_id missing".into());
  }

  let dir = tempfile::tempdir()?;
  let (config, workspace) = test_config(dir.path())?;
  let masker = Arc::new(Mutex::new(SecretMasker::new()));
  let mut ctx = ExecutionContext::with_masker(Arc::clone(&masker));

  // The collector timestamps each event AS IT ARRIVES, concurrently with the
  // running step — so a streamed mid-step Log lands before StepCompleted.
  let (tx, mut rx) = mpsc::channel::<RunnerEvent>(1024);
  let collector = tokio::spawn(async move {
    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
      events.push(Timed {
        at: Instant::now(),
        event,
      });
    }
    events
  });

  let spec = JobSpec::default();
  run_steps(
    &steps,
    &mut ctx,
    &tx,
    CancellationToken::new(),
    &JobRun {
      workspace: &workspace,
      config: &config,
      spec: &spec,
      shadow: None,
    },
  )
  .await?;

  drop(tx);
  let events = collector.await?;
  drop(dir);
  Ok((events, masker))
}

/// Arrival instant of the first plain (non-`##[`) stdout `Log` whose line
/// contains `needle`, for `step_id`.
fn first_log_at(events: &[Timed], step_id: &str, needle: &str) -> Option<Instant> {
  for t in events {
    if let RunnerEvent::Log {
      step_id: id,
      line,
      stream: LogStream::Stdout,
    } = &t.event
      && id == step_id
      && !line.starts_with("##[")
      && line.contains(needle)
    {
      return Some(t.at);
    }
  }
  None
}

/// Arrival instant of `StepCompleted` for `step_id`.
fn completed_at(events: &[Timed], step_id: &str) -> Option<Instant> {
  for t in events {
    if let RunnerEvent::StepCompleted { step_id: id, .. } = &t.event
      && id == step_id
    {
      return Some(t.at);
    }
  }
  None
}

/// All plain (non-`##[`) stdout log lines for `step_id`, in order.
fn plain_lines(events: &[Timed], step_id: &str) -> Vec<String> {
  let mut lines = Vec::new();
  for t in events {
    if let RunnerEvent::Log {
      step_id: id,
      line,
      stream: LogStream::Stdout,
    } = &t.event
      && id == step_id
      && !line.starts_with("##[")
    {
      lines.push(line.clone());
    }
  }
  lines
}

/// `StepCompleted.outputs` for `step_id`.
fn step_outputs(events: &[Timed], step_id: &str) -> std::collections::HashMap<String, String> {
  for t in events {
    if let RunnerEvent::StepCompleted {
      step_id: id,
      outputs,
      ..
    } = &t.event
      && id == step_id
    {
      return outputs.clone();
    }
  }
  std::collections::HashMap::new()
}

#[tokio::test]
async fn stdout_lines_stream_incrementally_before_step_completes() -> TestResult<()> {
  // The step emits line 1, sleeps, emits line 2, sleeps again. If stdout were
  // buffered until step end (the S7 regression), both Log events would land in
  // a tight burst just before StepCompleted. With realtime streaming, line 1's
  // Log arrives well BEFORE line 2's Log AND well before StepCompleted.
  let script = "\
echo line-one
sleep 0.6
echo line-two
sleep 0.6";
  let step = ActionStep::script("s1", script, "");
  let (events, _masker) = run_steps_timed(vec![step]).await?;

  let one = first_log_at(&events, "s1", "line-one").ok_or("line-one Log not emitted")?;
  let two = first_log_at(&events, "s1", "line-two").ok_or("line-two Log not emitted")?;
  let done = completed_at(&events, "s1").ok_or("StepCompleted not emitted")?;

  // line-one must be observed strictly before line-two (ordering preserved).
  assert!(one < two, "line-one must arrive before line-two");
  // line-one must arrive measurably before line-two (a streamed line, not a
  // post-completion burst). The script sleeps 0.6s between them; require a
  // conservative 250ms gap to absorb scheduler jitter.
  let gap = two.duration_since(one);
  assert!(
    gap >= std::time::Duration::from_millis(250),
    "line-one must stream before line-two's sleep elapses; gap was {gap:?}"
  );
  // line-one must also arrive well before the step completes (~1.2s of sleeps
  // remain after it). Require ≥250ms of slack.
  let lead = done.duration_since(one);
  assert!(
    lead >= std::time::Duration::from_millis(250),
    "line-one must stream before StepCompleted; lead was {lead:?}"
  );
  Ok(())
}

#[tokio::test]
async fn set_output_consumed_not_emitted_while_streaming() -> TestResult<()> {
  // `::set-output::` is a workflow command: it must be consumed (NOT logged as
  // a passthrough line) and surface in StepCompleted.outputs, even on the
  // streaming path.
  let script = "\
echo before
echo \"::set-output name=k::v\"
echo after";
  let step = ActionStep::script("s2", script, "");
  let (events, _masker) = run_steps_timed(vec![step]).await?;

  let lines = plain_lines(&events, "s2");
  assert!(
    lines.iter().any(|l| l == "before") && lines.iter().any(|l| l == "after"),
    "plain lines must stream through; lines={lines:?}"
  );
  assert!(
    !lines.iter().any(|l| l.contains("::set-output")),
    "::set-output:: must be consumed, not logged; lines={lines:?}"
  );
  let outputs = step_outputs(&events, "s2");
  assert_eq!(
    outputs.get("k").map(String::as_str),
    Some("v"),
    "::set-output:: must populate StepCompleted.outputs; outputs={outputs:?}"
  );
  Ok(())
}

#[tokio::test]
async fn add_mask_registered_before_later_line_is_streamed() -> TestResult<()> {
  // The dispatcher must apply `::add-mask::` BEFORE masking the line that
  // follows it, even though lines now flow one-at-a-time as the child runs.
  // A delay between the mask command and the secret line proves the mask is
  // registered on the streaming path (not retroactively at step end).
  let script = "\
echo \"::add-mask::s3cretValue\"
sleep 0.3
echo \"leaked s3cretValue here\"";
  let step = ActionStep::script("s3", script, "");
  let (events, masker) = run_steps_timed(vec![step]).await?;

  let lines = plain_lines(&events, "s3");
  let leaked = lines
    .iter()
    .find(|l| l.contains("leaked"))
    .ok_or("the output line was not streamed")?;
  assert_eq!(
    leaked, "leaked *** here",
    "secret emitted after ::add-mask:: must be masked on the streaming path; line={leaked:?}"
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
