//! Replay acquired issue-68 jobs through the real engine and shell/actions.

use std::error::Error;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use execution::Runner;
use execution::execution::job_runner::build_context;
use expressions::types::ExprValue;
use shared::{AgentJobRequestMessage, Conclusion, RunnerConfig, RunnerEvent, SecretMasker};
use tokio_util::sync::CancellationToken;

// Compare JSON representations without GitHub loose-equality coercion.
macro_rules! assert_value_eq {
  ($actual:expr, $expected:expr $(, $message:expr)? $(,)?) => {
    assert_eq!($actual.to_json_value(), $expected.to_json_value() $(, $message)?);
  };
}

type TestResult = Result<(), Box<dyn Error>>;
const MATRIX_0: &str = include_str!("incoming_contexts_matrix_0.json");
const MATRIX_1: &str = include_str!("incoming_contexts_matrix_1.json");
const CALL: &str = include_str!("incoming_contexts_call.json");

fn config(root: &std::path::Path) -> RunnerConfig {
  RunnerConfig {
    data_dir: root.join("data"),
    workspace_root: root.join("work"),
    workspace_gc_hours: 0,
    ..RunnerConfig::default()
  }
}

#[test]
fn acquired_contexts_retain_values_and_types() -> TestResult {
  for (source, tag, number, flag, index) in [
    (MATRIX_0, "alpha", 7.0, true, 0.0),
    (MATRIX_1, "beta", 0.0, false, 1.0),
  ] {
    let msg: AgentJobRequestMessage = serde_json::from_str(source)?;
    let dir = tempfile::tempdir()?;
    let ctx = build_context(
      &msg,
      &config(dir.path()),
      Arc::new(Mutex::new(SecretMasker::new())),
    );
    for (expr, expected) in [
      ("matrix.tag", ExprValue::String(tag.to_owned())),
      ("matrix.number", ExprValue::Number(number)),
      ("matrix.flag", ExprValue::Bool(flag)),
      (
        "needs.producer.outputs.artifact",
        ExprValue::String("artifact-68".to_owned()),
      ),
      (
        "needs.producer.result",
        ExprValue::String("success".to_owned()),
      ),
      ("inputs.who", ExprValue::String("world".to_owned())),
      ("inputs.count", ExprValue::String("3".to_owned())),
      ("inputs.enabled", ExprValue::Bool(false)),
      ("strategy.job-index", ExprValue::Number(index)),
      ("strategy.job-total", ExprValue::Number(2.0)),
      ("strategy.max-parallel", ExprValue::Number(1.0)),
      ("strategy.fail-fast", ExprValue::Bool(false)),
    ] {
      assert_value_eq!(ctx.evaluate_expression(expr)?, expected, "{expr}");
    }
  }
  Ok(())
}

// Boundary transformations keep the captured envelope and values. Only root
// presence/name changes; these are not additional server captures.
#[test]
fn absent_null_and_empty_roots_remain_distinct() -> TestResult {
  for root in ["matrix", "needs", "inputs", "strategy"] {
    for (replacement, expected) in [
      (None, ExprValue::Null),
      (Some(serde_json::Value::Null), ExprValue::Null),
      (
        Some(serde_json::json!({"t": 2, "d": []})),
        ExprValue::Object(std::collections::HashMap::new()),
      ),
    ] {
      let mut raw: serde_json::Value = serde_json::from_str(MATRIX_0)?;
      let contexts = raw
        .get_mut("contextData")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or("contextData missing")?;
      contexts.remove(root);
      if let Some(value) = replacement {
        contexts.insert(root.to_owned(), value);
      }
      let msg = serde_json::from_value(raw)?;
      let dir = tempfile::tempdir()?;
      let ctx = build_context(
        &msg,
        &config(dir.path()),
        Arc::new(Mutex::new(SecretMasker::new())),
      );
      assert_value_eq!(ctx.evaluate_expression(root)?, expected, "{root}");
      assert_value_eq!(
        ctx.evaluate_expression(&format!("{root}.missing"))?,
        ExprValue::Null
      );
    }
  }
  Ok(())
}

#[test]
fn new_roots_keep_nested_types_and_cannot_shadow_runtime_roots() -> TestResult {
  let mut msg: AgentJobRequestMessage = serde_json::from_str(MATRIX_1)?;
  let matrix = msg
    .context_data
    .get("matrix")
    .ok_or("matrix missing")?
    .clone();
  let github = msg
    .context_data
    .get("github")
    .ok_or("github missing")?
    .clone();
  msg
    .context_data
    .insert("FutureMatrix".to_owned(), matrix.clone());
  msg.context_data.insert("FutureNested".to_owned(), github);
  for root in ["EnV", "SeCrEtS", "StEpS", "RuNnEr", "JoB", "GiThUb", "VaRs"] {
    msg.context_data.insert(root.to_owned(), matrix.clone());
  }
  let dir = tempfile::tempdir()?;
  let ctx = build_context(
    &msg,
    &config(dir.path()),
    Arc::new(Mutex::new(SecretMasker::new())),
  );
  assert_value_eq!(
    ctx.evaluate_expression("futurematrix.number")?,
    ExprValue::Number(0.0)
  );
  assert_value_eq!(
    ctx.evaluate_expression("FUTUREMATRIX.flag")?,
    ExprValue::Bool(false)
  );
  assert_value_eq!(
    ctx.evaluate_expression("futurematrix.tag")?,
    ExprValue::String("beta".to_owned())
  );
  assert_value_eq!(
    ctx.evaluate_expression("futurenested.event.repository.topics")?,
    ExprValue::Array(Vec::new())
  );
  for root in ["env", "secrets", "steps", "runner", "job", "github", "vars"] {
    assert_value_eq!(
      ctx.evaluate_expression(&format!("{root}.tag"))?,
      ExprValue::Null,
      "{root}"
    );
  }
  assert_value_eq!(
    ctx.evaluate_expression("job.status")?,
    ExprValue::String("success".to_owned())
  );
  assert_value_eq!(
    ctx.evaluate_expression("runner.os")?,
    ExprValue::String(shared::platform::runner_os().to_owned())
  );
  assert_value_eq!(
    ctx.evaluate_expression("env.TOOLU_RUNNER_TOKEN")?,
    ExprValue::Null
  );
  assert_value_eq!(
    ctx.evaluate_expression("secrets.TOOLU_RUNNER_TOKEN")?,
    ExprValue::Null
  );
  assert_value_eq!(
    ctx.evaluate_expression("github.repository")?,
    ExprValue::String("Falconiere/toolu-ghrunner".to_owned())
  );
  Ok(())
}

#[tokio::test]
async fn acquired_matrix_jobs_execute_assertions_and_restore_composite_scope() -> TestResult {
  for source in [MATRIX_0, MATRIX_1] {
    replay(source, true).await?;
  }
  Ok(())
}

#[tokio::test]
async fn acquired_workflow_call_executes_typed_input_assertions() -> TestResult {
  replay(CALL, false).await
}

async fn replay(source: &str, matrix: bool) -> TestResult {
  let mut msg: AgentJobRequestMessage = serde_json::from_str(source)?;
  // Acquisition-only adaptation: prepopulate the exact checked-in actions.
  msg
    .steps
    .retain(|s| s.reference.name.as_deref() != Some("actions/checkout"));
  let dir = tempfile::tempdir()?;
  let cfg = config(dir.path());
  let workspace = cfg.workspace_root.join(&msg.job_id);
  for name in ["context-68-parent", "context-68-child"] {
    let dest = workspace.join(".github/actions").join(name);
    std::fs::create_dir_all(&dest)?;
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
      .join("../../.github/actions")
      .join(name)
      .join("action.yml");
    std::fs::copy(source, dest.join("action.yml"))?;
  }
  let expected_steps = msg.steps.len();
  let runner = Runner::new(cfg, Arc::new(Mutex::new(SecretMasker::new())));
  let cancel = CancellationToken::new();
  let mut events = runner.execute_job(msg, cancel.clone());
  let result = tokio::time::timeout(Duration::from_secs(60), async {
    let mut conclusion = None;
    let mut completed = 0;
    let mut logs = Vec::new();
    while let Some(event) = events.recv().await {
      match event {
        RunnerEvent::Log { line, .. } => logs.push(line),
        RunnerEvent::StepCompleted { conclusion, .. } => {
          assert_eq!(conclusion, Conclusion::Success, "{logs:?}");
          completed += 1;
        },
        RunnerEvent::JobCompleted {
          conclusion: value, ..
        } => conclusion = Some(value),
        RunnerEvent::JobStarted { .. }
        | RunnerEvent::StepStarted { .. }
        | RunnerEvent::StepSkipped { .. }
        | RunnerEvent::LogGroup { .. }
        | RunnerEvent::Annotation { .. } => {},
      }
    }
    assert_eq!(conclusion, Some(Conclusion::Success), "{logs:?}");
    assert_eq!(completed, expected_steps, "assertion steps must execute");
  })
  .await;
  cancel.cancel();
  result?;
  if matrix {
    for (file, expected) in [
      ("context-68.out", "contexts-ok"),
      ("context-68-world.out", "world"),
      ("context-68-sibling.out", "sibling"),
      ("context-68-child.out", "inner"),
    ] {
      assert_eq!(std::fs::read_to_string(workspace.join(file))?, expected);
    }
  } else {
    assert_eq!(
      std::fs::read_to_string(workspace.join("context-68-call.out"))?,
      "call-ok"
    );
  }
  Ok(())
}
