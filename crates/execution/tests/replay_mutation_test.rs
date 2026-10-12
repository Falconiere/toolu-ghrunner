//! Issue #103 S1: production replays must go red when message setup drops a
//! section.
//!
//! `compat_103_contexts_message.json` is the sanitized acquisition of the
//! reference lane's shell-only `contexts` job (run 38301583727, toolu lane). It
//! carries typed `inputs`, a `matrix`, workflow + job `env`, `defaults.run` and
//! job `outputs`, and its own steps assert every one of them. Each test below
//! replays the unmodified capture through the real `Runner::execute_job` (it
//! must pass) and then the same capture with exactly ONE section removed or
//! emptied (the named assertion step must fail, or the observable must change).
//! A helper that hand-builds contexts cannot satisfy this: the section is
//! dropped from the wire message itself.
//!
//! To prove a mutation fails for the right reason, every captured `test …`
//! assertion line is rewritten to `test … || (echo ASSERT_FAILED_<n>; exit 1)`.
//! The rewrite keeps the semantics (the step still stops at the first false
//! assertion) and names the exact line that tripped. Each mutation must trip
//! only the line that reads the dropped section. Parentheses rather than
//! braces keep the rewrite valid inside the captured `format()` expressions.
//!
//! Host requirements: `bash` and `jq` (the captured `Assert typed inputs` step
//! pipes `toJSON(inputs)` through `jq`). Both are present on the ubuntu and
//! macOS CI images; a missing `jq` fails fast with a named message.

use std::collections::HashMap;
use std::error::Error;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use execution::Runner;
use shared::{AgentJobRequestMessage, Conclusion, RunnerConfig, RunnerEvent, SecretMasker};
use tokio_util::sync::CancellationToken;

type TestResult = Result<(), Box<dyn Error>>;

const CONTEXTS: &str = include_str!("compat_103_contexts_message.json");
const JOB_OUTPUTS_70: &str = include_str!("job_outputs_message.json");

/// What a replay produced: the job conclusion, the `contextName`s of the steps
/// that failed, the evaluated job outputs and every step log line.
struct Replayed {
  conclusion: Conclusion,
  failed: Vec<String>,
  outputs: HashMap<String, String>,
  logs: Vec<String>,
}

fn require_jq() {
  let found = std::process::Command::new("jq")
    .arg("--version")
    .output()
    .is_ok_and(|out| out.status.success());
  assert!(
    found,
    "jq must be on PATH: the captured probe step pipes toJSON(inputs) through jq"
  );
}

/// Rewrite every captured `test …` script line so a false assertion also prints
/// `ASSERT_FAILED_<n>`; returns the original lines indexed by `n`.
fn instrument(message: &mut serde_json::Value) -> Result<Vec<String>, Box<dyn Error>> {
  let mut lines = Vec::new();
  let steps = message
    .get_mut("steps")
    .and_then(serde_json::Value::as_array_mut)
    .ok_or("captured steps missing")?;
  for step in steps {
    let Some(entries) = step
      .pointer_mut("/inputs/map")
      .and_then(serde_json::Value::as_array_mut)
    else {
      continue;
    };
    for entry in entries {
      if entry
        .pointer("/Key/lit")
        .and_then(serde_json::Value::as_str)
        != Some("script")
      {
        continue;
      }
      let value = object(entry.get_mut("Value").ok_or("script value missing")?)?;
      for field in ["lit", "expr"] {
        let Some(text) = value.get(field).and_then(serde_json::Value::as_str) else {
          continue;
        };
        let rewritten = text
          .split('\n')
          .map(|line| {
            if line.starts_with("test ") || line.starts_with("format('test ") {
              lines.push(line.to_owned());
              format!("{line} || (echo ASSERT_FAILED_{}; exit 1)", lines.len() - 1)
            } else {
              line.to_owned()
            }
          })
          .collect::<Vec<_>>()
          .join("\n");
        value.insert(field.to_owned(), serde_json::Value::String(rewritten));
      }
    }
  }
  Ok(lines)
}

/// The indices of the instrumented lines that tripped during a replay.
fn tripped(replayed: &Replayed) -> Vec<usize> {
  replayed
    .logs
    .iter()
    .filter_map(|line| line.trim().strip_prefix("ASSERT_FAILED_"))
    .filter_map(|n| n.parse().ok())
    .collect()
}

fn raw(source: &str) -> Result<serde_json::Value, Box<dyn Error>> {
  Ok(serde_json::from_str(source)?)
}

fn object(
  value: &mut serde_json::Value,
) -> Result<&mut serde_json::Map<String, serde_json::Value>, Box<dyn Error>> {
  value
    .as_object_mut()
    .ok_or_else(|| "expected a JSON object".into())
}

fn replay(raw: serde_json::Value) -> Result<Replayed, Box<dyn Error>> {
  let message: AgentJobRequestMessage = serde_json::from_value(raw)?;
  let names: HashMap<String, String> = message
    .steps
    .iter()
    .map(|step| {
      (
        step.id.clone(),
        step.context_name.clone().unwrap_or_default(),
      )
    })
    .collect();
  let dir = tempfile::tempdir()?;
  let config = RunnerConfig {
    data_dir: dir.path().join("data"),
    workspace_root: dir.path().join("work"),
    workspace_gc_hours: 0,
    workspace: shared::WorkspaceConfig {
      mode: shared::WorkspaceMode::Fresh,
      stable_gc_hours: 0,
    },
    ..RunnerConfig::default()
  };
  let runner = Runner::new(config, Arc::new(Mutex::new(SecretMasker::new())));
  let runtime = tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .build()?;
  runtime.block_on(async {
    let cancel = CancellationToken::new();
    let mut events = runner.execute_job(message, cancel.clone());
    let collected = tokio::time::timeout(Duration::from_secs(60), async {
      let mut failed = Vec::new();
      let mut logs = Vec::new();
      let mut completed = None;
      while let Some(event) = events.recv().await {
        match event {
          RunnerEvent::Log { line, .. } => logs.push(line),
          RunnerEvent::StepCompleted {
            step_id,
            conclusion: Conclusion::Failure,
            ..
          } => failed.push(names.get(&step_id).cloned().unwrap_or(step_id)),
          RunnerEvent::JobCompleted {
            conclusion,
            outputs,
            ..
          } => completed = Some((conclusion, outputs)),
          RunnerEvent::StepSummary { .. }
          | RunnerEvent::JobStarted { .. }
          | RunnerEvent::StepStarted { .. }
          | RunnerEvent::StepCompleted { .. }
          | RunnerEvent::StepSkipped { .. }
          | RunnerEvent::LogGroup { .. }
          | RunnerEvent::Annotation { .. }
          | RunnerEvent::StepMetadata { .. }
          | RunnerEvent::InfrastructureError { .. } => {},
        }
      }
      (failed, logs, completed)
    })
    .await;
    cancel.cancel();
    let (failed, logs, completed) = collected?;
    let (conclusion, outputs) = completed.ok_or("job never completed")?;
    Ok(Replayed {
      conclusion,
      failed,
      outputs,
      logs,
    })
  })
}

/// Replay the instrumented capture with `mutate` applied and assert that the
/// named step is the only failure and that the one assertion line that tripped
/// is the one reading the dropped section (it contains `marker`).
fn assert_mutation_fails_at(
  mutate: impl FnOnce(&mut serde_json::Value) -> TestResult,
  step: &str,
  marker: &str,
) -> TestResult {
  require_jq();
  let mut message = raw(CONTEXTS)?;
  let lines = instrument(&mut message)?;
  mutate(&mut message)?;
  let replayed = replay(message)?;
  assert_eq!(
    replayed.conclusion,
    Conclusion::Failure,
    "mutation must fail the job"
  );
  assert_eq!(replayed.failed, vec![step.to_owned()], "failing step");
  let tripped: Vec<&str> = tripped(&replayed)
    .into_iter()
    .filter_map(|n| lines.get(n).map(String::as_str))
    .collect();
  assert_eq!(
    tripped.len(),
    1,
    "exactly one assertion must trip: {tripped:?}"
  );
  assert!(
    tripped.iter().all(|line| line.contains(marker)),
    "the tripped assertion must read the dropped section ({marker}): {tripped:?}"
  );
  Ok(())
}

fn context_root(message: &mut serde_json::Value, root: &str) -> TestResult {
  let contexts = object(
    message
      .get_mut("contextData")
      .ok_or("contextData missing")?,
  )?;
  contexts
    .remove(root)
    .ok_or_else(|| format!("captured {root} missing"))?;
  Ok(())
}

#[test]
fn captured_contexts_job_replays_green_with_exact_outputs() -> TestResult {
  require_jq();
  // The instrumentation is semantics-preserving: the instrumented capture is
  // green too, and no assertion trips.
  let mut instrumented = raw(CONTEXTS)?;
  assert_eq!(
    instrument(&mut instrumented)?.len(),
    10,
    "captured assertion lines"
  );
  let replayed = replay(instrumented)?;
  assert_eq!(
    replayed.conclusion,
    Conclusion::Success,
    "failed: {:?}",
    replayed.failed
  );
  assert!(tripped(&replayed).is_empty());
  let replayed = replay(raw(CONTEXTS)?)?;
  assert_eq!(
    replayed.conclusion,
    Conclusion::Success,
    "failed: {:?}",
    replayed.failed
  );
  assert!(replayed.failed.is_empty());
  assert_eq!(
    replayed.outputs.get("marker").map(String::as_str),
    Some("contexts-103")
  );
  assert_eq!(
    replayed.outputs.get("who").map(String::as_str),
    Some("world")
  );
  Ok(())
}

#[test]
fn dropped_inputs_fail_the_typed_input_assertion() -> TestResult {
  assert_mutation_fails_at(|m| context_root(m, "inputs"), "__run_2", "= world")
}

#[test]
fn empty_who_input_fails_the_typed_input_assertion() -> TestResult {
  assert_mutation_fails_at(
    |message| {
      let inputs = message
        .pointer_mut("/contextData/inputs/d")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or("captured inputs missing")?;
      let who = inputs
        .iter_mut()
        .find(|entry| entry.get("k").and_then(serde_json::Value::as_str) == Some("who"))
        .ok_or("captured who missing")?;
      object(who)?.insert("v".to_owned(), serde_json::json!(""));
      Ok(())
    },
    "__run_2",
    "= world",
  )
}

#[test]
fn dropped_matrix_fails_the_context_assertion() -> TestResult {
  assert_mutation_fails_at(|m| context_root(m, "matrix"), "__run_3", "= alpha")
}

#[test]
fn dropped_workflow_and_job_env_fail_the_context_assertion() -> TestResult {
  assert_mutation_fails_at(
    |message| {
      object(message)?.insert("environmentVariables".to_owned(), serde_json::json!([]));
      Ok(())
    },
    "__run_3",
    "$WORKFLOW_LEVEL",
  )
}

#[test]
fn dropped_run_defaults_fail_the_context_assertion() -> TestResult {
  assert_mutation_fails_at(
    |message| {
      object(message)?.insert("defaults".to_owned(), serde_json::json!([]));
      Ok(())
    },
    "__run_3",
    "basename",
  )
}

#[test]
fn dropped_job_outputs_lose_the_evaluated_outputs() -> TestResult {
  for source in [CONTEXTS, JOB_OUTPUTS_70] {
    let intact = replay(raw(source)?)?;
    assert_eq!(intact.conclusion, Conclusion::Success);
    assert!(!intact.outputs.is_empty(), "captured job outputs evaluate");
    let mut message = raw(source)?;
    object(&mut message)?.insert("jobOutputs".to_owned(), serde_json::Value::Null);
    let dropped = replay(message)?;
    assert_eq!(dropped.conclusion, Conclusion::Success);
    assert!(
      dropped.outputs.is_empty(),
      "outputs must vanish: {:?}",
      dropped.outputs
    );
  }
  Ok(())
}
