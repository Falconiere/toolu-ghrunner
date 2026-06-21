//! Deadlock regression guard: a run-step must complete even when a grandchild
//! inherits and holds the stdout pipe open AFTER the immediate child exits.
//!
//! The realtime-streaming path forwards child stdout line-by-line on an `mpsc`
//! `Sender` that the step's command dispatcher drains via `recv()`; the
//! dispatcher (and the `tokio::join!` joining it with the child wait) only
//! completes when that `Sender` drops. The `Sender` drops only when the reader
//! task hits EOF on the pipe. But `cargo`/`bash` can spawn grandchildren
//! (build scripts, rustc proc-macro servers, backgrounded jobs) that INHERIT
//! the stdout pipe and outlive the immediate child, so the read never EOFs —
//! the reader blocks forever, the `Sender` never drops, `recv()` hangs, and the
//! step never completes.
//!
//! The fix bounds the post-exit drain: once `child.wait()` returns, the reader
//! gets a short grace period to finish already-buffered lines, then is aborted
//! so its `Sender` drops and the step completes. These tests drive REAL `bash`
//! steps through the REAL step loop (`execution::steps_runner::run_steps`) — no
//! mocks — and wrap each call in a hard `tokio::time::timeout` so a regression
//! to the deadlock fails the test instead of hanging the suite.

use std::error::Error;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use shared::{ActionStep, LogStream, RunnerConfig, RunnerEvent};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use toolu_runner::execution::context::ExecutionContext;
use toolu_runner::execution::job_spec::JobSpec;
use toolu_runner::execution::secret_masker::SecretMasker;
use toolu_runner::execution::steps_runner::{JobRun, run_steps};

type TestResult<T> = Result<T, Box<dyn Error>>;

/// Outer bound: if the step hangs (deadlock), the test fails here instead of
/// blocking the suite forever. Comfortably above the 2s drain grace.
const HARD_TIMEOUT: Duration = Duration::from_secs(10);

/// Build a throwaway `RunnerConfig` rooted under `dir` with its dirs created.
fn test_config(dir: &std::path::Path) -> TestResult<(RunnerConfig, std::path::PathBuf)> {
  let workspace = dir.join("work");
  std::fs::create_dir_all(&workspace)?;
  let config = RunnerConfig {
    data_dir: dir.join("data"),
    workspace_root: workspace.clone(),
    cgroup_path: None,
    services_mode: shared::ServicesMode::default(),
  };
  std::fs::create_dir_all(&config.data_dir)?;
  Ok((config, workspace))
}

/// Drive `steps` through the real step loop under a hard timeout, returning the
/// emitted events plus the `StepCompleted` conclusion for `step_id`. A timeout
/// (the deadlock) surfaces as an `Err`, never a hang.
async fn run_steps_bounded(
  steps: Vec<ActionStep>,
  step_id: &str,
) -> TestResult<(Vec<RunnerEvent>, shared::Conclusion)> {
  let dir = tempfile::tempdir()?;
  let (config, workspace) = test_config(dir.path())?;
  let masker = Arc::new(Mutex::new(SecretMasker::new()));
  let mut ctx = ExecutionContext::with_masker(masker);

  let (tx, mut rx) = mpsc::channel::<RunnerEvent>(1024);
  let collector = tokio::spawn(async move {
    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
      events.push(event);
    }
    events
  });

  let spec = JobSpec::default();
  let job_run = JobRun {
    workspace: &workspace,
    config: &config,
    spec: &spec,
  };
  let run = run_steps(&steps, &mut ctx, &tx, CancellationToken::new(), &job_run);
  // The whole step run is bounded; a deadlock trips the timeout instead of
  // hanging the test process indefinitely.
  tokio::time::timeout(HARD_TIMEOUT, run)
    .await
    .map_err(|elapsed| {
      format!("run_steps deadlocked: step did not complete in time ({elapsed})")
    })??;

  drop(tx);
  let events = collector.await?;
  drop(dir);
  let conclusion = conclusion_of(&events, step_id).ok_or("StepCompleted not emitted for step")?;
  Ok((events, conclusion))
}

/// The `StepCompleted` conclusion for `step_id`, if one was emitted.
fn conclusion_of(events: &[RunnerEvent], step_id: &str) -> Option<shared::Conclusion> {
  events.iter().find_map(|e| {
    if let RunnerEvent::StepCompleted {
      step_id: id,
      conclusion,
      ..
    } = e
      && id == step_id
    {
      return Some(*conclusion);
    }
    None
  })
}

/// All plain (non-`##[`) stdout log lines for `step_id`, in order.
fn plain_stdout_lines(events: &[RunnerEvent], step_id: &str) -> Vec<String> {
  events
    .iter()
    .filter_map(|e| {
      if let RunnerEvent::Log {
        step_id: id,
        line,
        stream: LogStream::Stdout,
      } = e
        && id == step_id
        && !line.starts_with("##[")
      {
        return Some(line.clone());
      }
      None
    })
    .collect()
}

/// `StepCompleted.outputs` for `step_id`.
fn step_outputs(
  events: &[RunnerEvent],
  step_id: &str,
) -> std::collections::HashMap<String, String> {
  for e in events {
    if let RunnerEvent::StepCompleted {
      step_id: id,
      outputs,
      ..
    } = e
      && id == step_id
    {
      return outputs.clone();
    }
  }
  std::collections::HashMap::new()
}

/// THE DEADLOCK REPRO: a run-step whose script backgrounds a `sleep` that
/// INHERITS stdout and outlives the immediate `bash`. Before the fix, the
/// reader blocks on the still-open pipe, its `Sender` never drops, the
/// dispatcher's `recv()` hangs, `tokio::join!` deadlocks, and the step never
/// completes — `run_steps_bounded` times out. After the fix, the child `bash`
/// exits immediately, the 2s drain grace bounds the orphaned reader, and the
/// step completes well under the 10s hard timeout even though the grandchild
/// `sleep` still holds the pipe.
#[tokio::test]
async fn grandchild_holding_stdout_does_not_deadlock_step() -> TestResult<()> {
  // `sleep 30 &` inherits stdout and lives 30s after bash exits; `bash` itself
  // returns the instant it has launched the background job and echoed.
  let script = "\
sleep 30 &
echo done";
  let step = ActionStep::script("dl", script, "");

  let start = std::time::Instant::now();
  let (events, conclusion) = run_steps_bounded(vec![step], "dl").await?;
  let elapsed = start.elapsed();

  assert_no_deadlock(&events, conclusion, elapsed, "dl");
  Ok(())
}

/// Assert the grandchild repro completed cleanly: returned (no hang), step
/// succeeded, finished on the 2s drain grace (not the grandchild's 30s nor the
/// 10s hard timeout that would fire on the un-fixed deadlock), pre-exit line
/// captured.
fn assert_no_deadlock(
  events: &[RunnerEvent],
  conclusion: shared::Conclusion,
  elapsed: Duration,
  step_id: &str,
) {
  assert_eq!(
    conclusion,
    shared::Conclusion::Success,
    "step must complete successfully despite the lingering grandchild"
  );
  assert!(
    elapsed < Duration::from_secs(8),
    "step should finish on the drain grace, not wait on the grandchild; took {elapsed:?}"
  );
  let lines = plain_stdout_lines(events, step_id);
  assert!(
    lines.iter().any(|l| l == "done"),
    "the pre-exit `echo done` line must be captured by the bounded drain; lines={lines:?}"
  );
}

/// NO REGRESSION: a normal run-step (no lingering grandchild) still completes
/// and captures `::set-output::` plus plain stdout correctly. Proves the
/// drain-before-abort preserves output processing on the happy path.
#[tokio::test]
async fn normal_step_completes_and_captures_output() -> TestResult<()> {
  let script = "\
echo before
echo \"::set-output name=k::v\"
echo after";
  let step = ActionStep::script("ok", script, "");

  let (events, conclusion) = run_steps_bounded(vec![step], "ok").await?;
  assert_eq!(conclusion, shared::Conclusion::Success);

  let lines = plain_stdout_lines(&events, "ok");
  assert!(
    lines.iter().any(|l| l == "before") && lines.iter().any(|l| l == "after"),
    "plain stdout must stream through; lines={lines:?}"
  );
  assert!(
    !lines.iter().any(|l| l.contains("::set-output")),
    "::set-output:: must be consumed, not logged; lines={lines:?}"
  );

  let outputs = step_outputs(&events, "ok");
  assert_eq!(
    outputs.get("k").map(String::as_str),
    Some("v"),
    "::set-output:: must populate StepCompleted.outputs; outputs={outputs:?}"
  );
  Ok(())
}
