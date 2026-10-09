//! Replay the issue-99 step-attribute captures (`step-attrs-99.yml`, run
//! 37992025403, toolu lane) through the real engine and real `bash`.
//!
//! The fixtures are the acquired job messages with secret variables and mask
//! values replaced by deterministic version-5 UUID placeholders and the
//! endpoint `AccessToken` by `redacted`. The only replay adaptation is selecting a subset of the
//! captured steps by `contextName`, so each scenario's assertion steps run
//! next to the steps they check. Expected diagnostics are the reference
//! runner's log lines from the same run.

use std::collections::HashMap;
use std::error::Error;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use execution::Runner;
use shared::{AgentJobRequestMessage, Conclusion, RunnerConfig, RunnerEvent, SecretMasker};
use tokio_util::sync::CancellationToken;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;
const ATTRS: &str = include_str!("step_attrs_99_attrs.json");
const INVALID: &str = include_str!("step_attrs_99_invalid.json");
/// The local action the captured `composite_budget` step uses, seeded into
/// the replay workspace in place of the job's checkout.
const COMPOSITE_ACTION: &str =
  include_str!("../../../.github/actions/step-attrs-99-composite/action.yml");

/// Per-`contextName` results of one replayed job.
struct Replay {
  job: Option<Conclusion>,
  conclusions: HashMap<String, Conclusion>,
  logs: HashMap<String, Vec<String>>,
  /// The `StepStarted` display name.
  started: HashMap<String, String>,
}

impl Replay {
  fn conclusion(&self, name: &str) -> Option<Conclusion> {
    self.conclusions.get(name).copied()
  }

  fn logs(&self, name: &str) -> &[String] {
    self.logs.get(name).map_or(&[], Vec::as_slice)
  }
}

/// Keep the captured steps whose `contextName` is in `names`, in order.
fn select(source: &str, names: &[&str]) -> TestResult<AgentJobRequestMessage> {
  let mut msg: AgentJobRequestMessage = serde_json::from_str(source)?;
  msg.steps.retain(|step| {
    step
      .context_name
      .as_deref()
      .is_some_and(|name| names.contains(&name))
  });
  if msg.steps.len() != names.len() {
    return Err(
      format!(
        "expected {} captured steps, kept {}",
        names.len(),
        msg.steps.len()
      )
      .into(),
    );
  }
  Ok(msg)
}

fn replay(msg: AgentJobRequestMessage) -> TestResult<Replay> {
  let names: HashMap<String, String> = msg
    .steps
    .iter()
    .filter_map(|step| Some((step.id.clone(), step.context_name.clone()?)))
    .collect();
  let name_of = |step_id: &str| {
    names
      .get(step_id)
      .cloned()
      .unwrap_or_else(|| step_id.to_owned())
  };
  let dir = tempfile::tempdir()?;
  let config = RunnerConfig {
    data_dir: dir.path().join("data"),
    workspace_root: dir.path().join("work"),
    workspace_gc_hours: 0,
    ..RunnerConfig::default()
  };
  let action_dir = config
    .workspace_root
    .join(&msg.job_id)
    .join(".github/actions/step-attrs-99-composite");
  std::fs::create_dir_all(&action_dir)?;
  std::fs::write(action_dir.join("action.yml"), COMPOSITE_ACTION)?;
  let runner = Runner::new(config, Arc::new(Mutex::new(SecretMasker::new())));
  let cancel = CancellationToken::new();
  let runtime = tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .build()?;
  let mut replay = Replay {
    job: None,
    conclusions: HashMap::new(),
    logs: HashMap::new(),
    started: HashMap::new(),
  };
  runtime.block_on(async {
    let mut events = runner.execute_job(msg, cancel.clone());
    let drained = tokio::time::timeout(Duration::from_secs(150), async {
      while let Some(event) = events.recv().await {
        match event {
          RunnerEvent::Log { step_id, line, .. } => {
            let name = name_of(&step_id);
            replay.logs.entry(name).or_default().push(line);
          },
          RunnerEvent::StepCompleted {
            step_id,
            conclusion,
            ..
          } => {
            let name = name_of(&step_id);
            replay.conclusions.insert(name, conclusion);
          },
          RunnerEvent::StepStarted {
            step_id, step_name, ..
          } => {
            let name = names.get(&step_id).cloned().unwrap_or(step_id);
            replay.started.insert(name, step_name);
          },
          RunnerEvent::JobCompleted { conclusion, .. } => replay.job = Some(conclusion),
          RunnerEvent::JobStarted { .. }
          | RunnerEvent::StepSkipped { .. }
          | RunnerEvent::LogGroup { .. }
          | RunnerEvent::StepSummary { .. }
          | RunnerEvent::Annotation { .. } => {},
        }
      }
    })
    .await;
    cancel.cancel();
    drained
  })?;
  Ok(replay)
}

fn assert_all_succeeded(replay: &Replay, names: &[&str]) {
  for name in names {
    assert_eq!(
      replay.conclusion(name),
      Some(Conclusion::Success),
      "{name}: {:?}",
      replay.logs(name)
    );
  }
  assert_eq!(replay.job, Some(Conclusion::Success), "{:?}", replay.logs);
}

/// S1/S2: literal, deferred (`steps.*`) and matrix `continue-on-error` all
/// split failure outcome from success conclusion; literal `false`/`0` and a
/// negative deferred timeout leave the step unbounded and green. The
/// captured `Check attribute results` step asserts the outcome/conclusion
/// pairs from inside the job.
#[test]
fn captured_literal_deferred_and_matrix_attributes() -> TestResult {
  let names = [
    "prior",
    "literal_coe",
    "deferred_coe",
    "matrix_attrs",
    "literal_false",
    "negative_timeout",
    "__run",
  ];
  let replay = replay(select(ATTRS, &names)?)?;
  assert_all_succeeded(&replay, &names);
  Ok(())
}

/// S4: display names match the reference lane's jobs-API names (its matrix
/// lane swapped in): `steps.*` names evaluate before the step runs, a name
/// that fails there keeps its prettified job-start text, and one that
/// already failed at job start reports `run`. Each failure logs upstream's
/// warning where the reference did: "Set up job", then the step itself.
#[test]
fn captured_display_names_match_the_reference() -> TestResult {
  let steps = [
    "prior",
    "deferred_coe",
    "matrix_attrs",
    "__run_7",
    "__run_8",
    "__run_9",
    "__run_10",
  ];
  let replay = replay(select(ATTRS, &steps)?)?;
  assert_all_succeeded(&replay, &steps);
  let expected = [
    ("prior", "Prior outputs"),
    ("deferred_coe", "Deferred from-prior"),
    ("matrix_attrs", "Matrix toolu-linux input "),
    ("__run_7", "Bad ${{ fromJSON(steps.prior.outputs.bad) }}"),
    ("__run_8", "run"),
    ("__run_9", "Run echo lane toolu-linux"),
    ("__run_10", "Run echo prior from-prior"),
  ];
  for (step, name) in expected {
    assert_eq!(
      replay.started.get(step).map(String::as_str),
      Some(name),
      "{step}"
    );
  }
  let bad = "##[warning]Encountered an error when evaluating display name ${{ format('Bad {0}', \
             fromJSON(steps.prior.outputs.bad)) }}. The template is not valid. \
             .github/workflows/step-attrs-99.yml (Line: 154, Col: 15): ";
  let nope = "##[warning]Encountered an error when evaluating display name ${{ \
              fromJSON('nope') }}. The template is not valid. \
              .github/workflows/step-attrs-99.yml (Line: 157, Col: 15): ";
  for (step, warning) in [
    (shared::SETUP_STEP_ID, nope),
    ("__run_7", bad),
    ("__run_8", nope),
  ] {
    let logs = replay.logs(step);
    assert!(
      logs.iter().any(|line| line.starts_with(warning)),
      "{step}: {logs:?}"
    );
  }
  Ok(())
}

/// S2: `timeout-minutes: ${{ fromJSON(steps.prior.outputs.minutes) }}` kills
/// a real `sleep 600` after one minute; the captured check asserts the
/// elapsed window and failure/success split.
#[test]
fn captured_deferred_timeout_bounds_a_real_process() -> TestResult {
  let names = ["prior", "deferred_start", "deferred_timeout", "__run_2"];
  let replay = replay(select(ATTRS, &names)?)?;
  assert_all_succeeded(&replay, &names);
  assert_timeout_line(
    &replay,
    "deferred_timeout",
    "##[error]The action 'Deferred timeout' has timed out after 1 minutes.",
  );
  Ok(())
}

/// The reference runner's timeout line for `name`, logged exactly once.
fn assert_timeout_line(replay: &Replay, name: &str, expected: &str) {
  let timeouts: Vec<&String> = replay
    .logs(name)
    .iter()
    .filter(|line| line.contains("timed out"))
    .collect();
  assert_eq!(timeouts, [expected], "{:?}", replay.logs(name));
}

/// S4: composite children share the step's one-minute budget. The second
/// inner `sleep 40` is killed with upstream's nested message, and the
/// composite step itself adds no top-level timeout line.
#[test]
fn captured_composite_children_share_one_budget() -> TestResult {
  let names = ["composite_start", "composite_budget", "__run_5"];
  let replay = replay(select(ATTRS, &names)?)?;
  assert_all_succeeded(&replay, &names);
  assert_timeout_line(
    &replay,
    "composite_budget",
    "##[error]The action has timed out.",
  );
  Ok(())
}

/// S2: `timeout-minutes: 1.9` truncates to one minute like upstream's
/// `(Int32)` cast.
#[test]
fn captured_fractional_timeout_truncates_to_whole_minutes() -> TestResult {
  let names = ["fractional_start", "fractional_timeout", "__run_3"];
  let replay = replay(select(ATTRS, &names)?)?;
  assert_all_succeeded(&replay, &names);
  assert_timeout_line(
    &replay,
    "fractional_timeout",
    "##[error]The action 'Fractional timeout' has timed out after 1 minutes.",
  );
  Ok(())
}

/// S3: a string timeout logs upstream's two error lines and applies no
/// bound — the captured `sleep 70` outlives what a 1-minute timeout allows.
#[test]
fn captured_string_timeout_logs_error_and_applies_no_bound() -> TestResult {
  let names = ["prior", "string_timeout", "__run_4"];
  let replay = replay(select(ATTRS, &names)?)?;
  assert_all_succeeded(&replay, &names);
  let head: Vec<&str> = replay
    .logs("string_timeout")
    .iter()
    .take(2)
    .map(String::as_str)
    .collect();
  assert_eq!(
    head,
    [
      "##[error]An error occurred when attempting to determine the step timeout.",
      "##[error]The template is not valid. .github/workflows/step-attrs-99.yml (Line: 118, Col: 26): Unexpected value '1'",
    ]
  );
  Ok(())
}

/// S3: the whole captured `invalid` job. A string `continue-on-error` logs
/// upstream's two error lines and the failed step stays failed; the
/// `always()` check sees failure/failure and the job fails.
#[test]
fn captured_string_continue_on_error_keeps_the_failure() -> TestResult {
  let replay = replay(serde_json::from_str(INVALID)?)?;
  assert_eq!(replay.conclusion("prior"), Some(Conclusion::Success));
  assert_eq!(replay.conclusion("string_coe"), Some(Conclusion::Failure));
  assert_eq!(
    replay.conclusion("__run"),
    Some(Conclusion::Success),
    "{:?}",
    replay.logs("__run")
  );
  assert_eq!(replay.job, Some(Conclusion::Failure));
  let logs = replay.logs("string_coe");
  let tail: Vec<&str> = logs
    .iter()
    .rev()
    .take(2)
    .rev()
    .map(String::as_str)
    .collect();
  assert_eq!(
    tail,
    [
      "##[error]The step failed and an error occurred when attempting to determine whether to continue on error.",
      "##[error]The template is not valid. .github/workflows/step-attrs-99.yml (Line: 185, Col: 28): Unexpected value 'true'",
    ]
  );
  Ok(())
}
