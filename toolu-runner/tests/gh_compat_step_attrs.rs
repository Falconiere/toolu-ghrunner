//! AC-7: per-step attribute fidelity — `timeout-minutes`, `working-directory`,
//! `continue-on-error` (outcome vs conclusion), and the `INPUT_` env transform.
//!
//! Drives real `bash` script steps through the real step loop
//! (`execution::steps_runner::run_steps`) — no mocks — and asserts the four
//! behaviors match the upstream GitHub Actions runner. The timeout path is
//! exercised against a real child process via the `step_timeout::wait_bounded`
//! seam (whole-minute `timeout-minutes` can't be waited on in a unit test, so a
//! tiny injected `Duration` stands in for the computed bound). The committed
//! `job_message.json` fixture is loaded to confirm it deserializes into the
//! `AgentJobRequestMessage` shape the engine consumes.

use std::error::Error;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use shared::{
  ActionStep, ActionStepDefinitionReference, AgentJobRequestMessage, Conclusion, DictEntry,
  RunnerConfig, RunnerEvent, TemplateToken,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use toolu_runner::execution::context::ExecutionContext;
use toolu_runner::execution::handlers::node::input_env_key;
use toolu_runner::execution::secret_masker::SecretMasker;
use toolu_runner::execution::step_timeout::{WaitOutcome, timeout_duration, wait_bounded};
use toolu_runner::execution::steps_runner::run_steps;

const JOB_MESSAGE: &str = include_str!("fixtures/job_message.json");

type TestResult<T> = Result<T, Box<dyn Error>>;

/// Confirm the committed fixture deserializes into the engine's message type.
fn fixture_job() -> TestResult<AgentJobRequestMessage> {
  Ok(serde_json::from_str(JOB_MESSAGE)?)
}

/// A run-step `with:` input as a literal key/value pair.
fn lit_entry(key: &str, value: &str) -> DictEntry<TemplateToken> {
  DictEntry {
    key: TemplateToken {
      token_type: 0,
      lit: Some(key.to_owned()),
      ..TemplateToken::default()
    },
    value: TemplateToken {
      token_type: 0,
      lit: Some(value.to_owned()),
      ..TemplateToken::default()
    },
  }
}

/// Build a `run:` step whose inputs carry `script`, `bash` shell, and the
/// optional `workingDirectory` (the wire key for `working-directory:`).
fn script_step(id: &str, body: &str, working_dir: Option<&str>) -> ActionStep {
  let mut entries = vec![lit_entry("script", body), lit_entry("shell", "bash")];
  if let Some(wd) = working_dir {
    entries.push(lit_entry("workingDirectory", wd));
  }
  ActionStep {
    id: id.to_owned(),
    step_type: Some("script".to_owned()),
    display_name_token: None,
    context_name: Some(id.to_owned()),
    condition: None,
    continue_on_error: None,
    timeout_in_minutes: None,
    reference: ActionStepDefinitionReference::script(),
    inputs: TemplateToken {
      token_type: 2,
      d: Some(entries),
      ..TemplateToken::default()
    },
    environment: None,
  }
}

/// Per-run result: the collected events plus the live `ExecutionContext` so the
/// `steps.<id>.outcome` / `.conclusion` expression context can be inspected.
struct RunResult {
  events: Vec<RunnerEvent>,
  ctx: ExecutionContext,
}

/// Drive `steps` through the real step loop, returning every emitted event and
/// the final context. `workspace_setup` runs before the steps (e.g. to create
/// a `working-directory` subdir under the fixture workspace).
async fn run_steps_collect(
  steps: Vec<ActionStep>,
  workspace_setup: impl FnOnce(&std::path::Path) -> std::io::Result<()>,
) -> TestResult<RunResult> {
  if fixture_job()?.job_id.is_empty() {
    return Err("fixture job_id missing".into());
  }

  let dir = tempfile::tempdir()?;
  let workspace = dir.path().join("work");
  std::fs::create_dir_all(&workspace)?;
  workspace_setup(&workspace)?;
  let config = RunnerConfig {
    data_dir: dir.path().join("data"),
    workspace_root: workspace.clone(),
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
  Ok(RunResult { events, ctx })
}

/// The `StepCompleted` conclusion for `step_id`, if any.
fn step_conclusion(events: &[RunnerEvent], step_id: &str) -> Option<Conclusion> {
  events.iter().find_map(|e| {
    if let RunnerEvent::StepCompleted {
      step_id: id,
      conclusion,
      ..
    } = e
      && id == step_id
    {
      Some(*conclusion)
    } else {
      None
    }
  })
}

/// All log lines emitted for `step_id`.
fn step_logs(events: &[RunnerEvent], step_id: &str) -> Vec<String> {
  events
    .iter()
    .filter_map(|e| {
      if let RunnerEvent::Log {
        step_id: id, line, ..
      } = e
        && id == step_id
      {
        Some(line.clone())
      } else {
        None
      }
    })
    .collect()
}

/// Read `steps.<id>.<field>` from the live expression context.
fn steps_field(ctx: &ExecutionContext, id: &str, field: &str) -> Option<String> {
  use toolu_runner::execution::expressions::types::ExprValue;
  let expr = format!("steps.{id}.{field}");
  if let Ok(ExprValue::String(s)) = ctx.evaluate_expression(&expr) {
    Some(s)
  } else {
    None
  }
}

// ---------------------------------------------------------------------------
// timeout-minutes
// ---------------------------------------------------------------------------

/// `timeout-minutes` maps to a whole-minute `Duration` (or unbounded).
#[test]
fn timeout_duration_is_whole_minutes() {
  assert_eq!(timeout_duration(Some(5)), Some(Duration::from_secs(300)));
  assert_eq!(timeout_duration(Some(1)), Some(Duration::from_secs(60)));
  assert_eq!(timeout_duration(Some(0)), None);
  assert_eq!(timeout_duration(None), None);
}

/// A real child that exceeds its (tiny, injected) timeout bound is KILLED and
/// the wait returns `TimedOut` promptly — never hanging for the wall-clock
/// `timeout-minutes`. This is the seam the script/node handlers run on.
#[tokio::test]
async fn long_child_is_killed_on_timeout() -> TestResult<()> {
  let mut child = tokio::process::Command::new("bash")
    .args(["-c", "sleep 30"])
    .spawn()?;

  let start = Instant::now();
  let outcome = wait_bounded(
    &mut child,
    Some(Duration::from_millis(200)),
    &CancellationToken::new(),
    shared::RunnerError::ScriptHandler,
  )
  .await?;

  assert!(
    matches!(outcome, WaitOutcome::TimedOut),
    "expected TimedOut"
  );
  assert!(
    start.elapsed() < Duration::from_secs(5),
    "wait should return at the bound, not wait out the sleep"
  );
  Ok(())
}

/// A fired `CancellationToken` kills the in-flight child and yields `Cancelled`.
#[tokio::test]
async fn running_child_is_killed_on_cancel() -> TestResult<()> {
  let mut child = tokio::process::Command::new("bash")
    .args(["-c", "sleep 30"])
    .spawn()?;

  let cancel = CancellationToken::new();
  let cancel2 = cancel.clone();
  tokio::spawn(async move {
    tokio::time::sleep(Duration::from_millis(150)).await;
    cancel2.cancel();
  });

  let outcome = wait_bounded(
    &mut child,
    None,
    &cancel,
    shared::RunnerError::ScriptHandler,
  )
  .await?;
  assert!(
    matches!(outcome, WaitOutcome::Cancelled),
    "expected Cancelled"
  );
  Ok(())
}

/// A child that finishes within its timeout reports its real exit status.
#[tokio::test]
async fn fast_child_reports_exit() -> TestResult<()> {
  let mut child = tokio::process::Command::new("bash")
    .args(["-c", "exit 0"])
    .spawn()?;
  let outcome = wait_bounded(
    &mut child,
    Some(Duration::from_secs(60)),
    &CancellationToken::new(),
    shared::RunnerError::ScriptHandler,
  )
  .await?;
  if let WaitOutcome::Exited(status) = outcome {
    assert!(status.success());
  } else {
    return Err("expected Exited".into());
  }
  Ok(())
}

// ---------------------------------------------------------------------------
// working-directory
// ---------------------------------------------------------------------------

/// `working-directory` is observable: a `pwd` step's stdout ends in `/sub`.
#[tokio::test]
async fn working_directory_pwd_ends_in_sub() -> TestResult<()> {
  let step = script_step("s1", "pwd", Some("sub"));
  let result = run_steps_collect(vec![step], |ws| std::fs::create_dir_all(ws.join("sub"))).await?;

  let outcome = steps_field(&result.ctx, "s1", "outcome");
  assert_eq!(outcome.as_deref(), Some("success"));
  let logs = step_logs(&result.events, "s1");
  let pwd = logs
    .iter()
    .find(|l| l.contains('/') && !l.starts_with("##["))
    .cloned()
    .unwrap_or_default();
  assert!(
    pwd.ends_with("/sub"),
    "pwd should end in /sub, got {logs:?}"
  );
  Ok(())
}

/// No `working-directory` keeps the workspace root as cwd (no `/sub` suffix).
#[tokio::test]
async fn no_working_directory_uses_workspace_root() -> TestResult<()> {
  let step = script_step("s1", "pwd", None);
  let result = run_steps_collect(vec![step], |_ws| Ok(())).await?;
  let logs = step_logs(&result.events, "s1");
  let pwd = logs
    .iter()
    .find(|l| l.starts_with('/'))
    .cloned()
    .unwrap_or_default();
  assert!(
    pwd.ends_with("/work"),
    "pwd should be workspace root, got {logs:?}"
  );
  Ok(())
}

// ---------------------------------------------------------------------------
// continue-on-error: outcome != conclusion
// ---------------------------------------------------------------------------

/// A failing step with `continue-on-error: true` has `outcome == failure` but
/// `conclusion == success`, and a following step still runs.
#[tokio::test]
async fn continue_on_error_splits_outcome_and_conclusion() -> TestResult<()> {
  let mut failing = script_step("bad", "exit 1", None);
  failing.continue_on_error = Some(true);
  let next = script_step("good", "echo ran", None);

  let result = run_steps_collect(vec![failing, next], |_ws| Ok(())).await?;

  // StepCompleted reports the continue-on-error-adjusted conclusion.
  assert_eq!(
    step_conclusion(&result.events, "bad"),
    Some(Conclusion::Success),
    "conclusion should be success so the job proceeds"
  );

  // Expression context exposes BOTH, and they differ.
  assert_eq!(
    steps_field(&result.ctx, "bad", "outcome").as_deref(),
    Some("failure"),
    "outcome is the real result"
  );
  assert_eq!(
    steps_field(&result.ctx, "bad", "conclusion").as_deref(),
    Some("success"),
    "conclusion is continue-on-error-adjusted"
  );

  // The following step still ran.
  assert_eq!(
    step_conclusion(&result.events, "good"),
    Some(Conclusion::Success)
  );
  assert!(
    step_logs(&result.events, "good").iter().any(|l| l == "ran"),
    "next step should have executed"
  );
  Ok(())
}

/// Without `continue-on-error`, a failing step's outcome AND conclusion are
/// both `failure`.
#[tokio::test]
async fn failure_without_continue_on_error_matches() -> TestResult<()> {
  let failing = script_step("bad", "exit 1", None);
  let result = run_steps_collect(vec![failing], |_ws| Ok(())).await?;

  assert_eq!(
    step_conclusion(&result.events, "bad"),
    Some(Conclusion::Failure)
  );
  assert_eq!(
    steps_field(&result.ctx, "bad", "outcome").as_deref(),
    Some("failure")
  );
  assert_eq!(
    steps_field(&result.ctx, "bad", "conclusion").as_deref(),
    Some("failure")
  );
  Ok(())
}

// ---------------------------------------------------------------------------
// INPUT_ transform
// ---------------------------------------------------------------------------

/// An input named `my input` becomes env key `INPUT_MY_INPUT` (whitespace runs
/// collapse to a single `_`, then uppercased) — matching the official runner.
#[test]
fn input_env_key_collapses_whitespace() {
  assert_eq!(input_env_key("my input"), "INPUT_MY_INPUT");
  assert_eq!(input_env_key("my   input"), "INPUT_MY_INPUT");
  assert_eq!(input_env_key("Multi Word Name"), "INPUT_MULTI_WORD_NAME");
  // Hyphens preserved, single words uppercased.
  assert_eq!(input_env_key("fetch-depth"), "INPUT_FETCH-DEPTH");
  assert_eq!(input_env_key("name"), "INPUT_NAME");
  // Leading/trailing whitespace does not produce empty segments.
  assert_eq!(input_env_key("  spaced  "), "INPUT_SPACED");
}
