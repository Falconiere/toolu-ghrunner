//! Issue 69 real Node and nested-composite environment probes.
//!
//! This transforms the captured #68 job message only to select committed local
//! actions and explicit environment layers. It drives [`crate::Runner`] without
//! a mocked action, shell, or Node runtime.

use std::error::Error;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::node::runtime::{node_binary_path, node_cache_dir, node_version_for};
use shared::{AgentJobRequestMessage, Conclusion, RunnerConfig, RunnerEvent, SecretMasker};
use tokio_util::sync::CancellationToken;

type TestResult = Result<(), Box<dyn Error>>;
const CAPTURE: &str = include_str!("../../../tests/incoming_contexts_matrix_0.json");
const LAYERS: &str = include_str!("../../../tests/job_environment_layers.json");

fn literal(value: &str) -> serde_json::Value {
  serde_json::json!({"type": 0, "lit": value})
}

fn mapping(values: &[(&str, serde_json::Value)]) -> serde_json::Value {
  serde_json::json!({"type": 2, "map": values.iter().map(|(key, value)|
    serde_json::json!({"Key": literal(key), "Value": value})).collect::<Vec<_>>()})
}

fn action_step(
  template: &serde_json::Value,
  action: &str,
  marker: &str,
  shared: &str,
) -> Result<serde_json::Value, Box<dyn Error>> {
  let mut step = template.as_object().ok_or("action step object")?.clone();
  step.insert(
    "id".to_owned(),
    serde_json::json!(uuid::Uuid::new_v4().to_string()),
  );
  step.insert(
    "contextName".to_owned(),
    serde_json::json!(format!("env_69_{marker}")),
  );
  step.insert(
    "reference".to_owned(),
    serde_json::json!({
      "path": format!("./.github/actions/{action}"), "repositoryType": "self", "type": "repository"
    }),
  );
  step.insert(
    "environment".to_owned(),
    mapping(&[("SHARED", literal(shared))]),
  );
  step.insert(
    "inputs".to_owned(),
    mapping(&[
      ("marker", literal(marker)),
      ("expected-shared", literal(shared)),
      ("expected-empty", literal("")),
      (
        "expected-secret",
        serde_json::json!({"type":3,"expr":"env.SECRET_VALUE"}),
      ),
      ("expected-multiline", literal("first\nsecond\n")),
      (
        "expected-literal",
        serde_json::json!({"type":3,"expr":"env.LITERAL"}),
      ),
    ]),
  );
  Ok(serde_json::Value::Object(step))
}

fn script_step(template: &serde_json::Value) -> Result<serde_json::Value, Box<dyn Error>> {
  let mut step = template.as_object().ok_or("script step object")?.clone();
  step.insert(
    "id".to_owned(),
    serde_json::json!(uuid::Uuid::new_v4().to_string()),
  );
  step.insert("contextName".to_owned(), serde_json::json!("env_69_verify"));
  step.insert("inputs".to_owned(), mapping(&[("script", literal(
    "test \"$ACTION_FILE\" = from-second-nested\ntest \"$COMPOSITE_FILE\" = from-second\ntest \"$SHARED\" = job\n"
  ))]));
  Ok(serde_json::Value::Object(step))
}

fn write_actions(workspace: &Path) -> Result<(), Box<dyn Error>> {
  let node = workspace.join(".github/actions/env-69-node");
  let composite = workspace.join(".github/actions/env-69-composite");
  std::fs::create_dir_all(&node)?;
  std::fs::create_dir_all(&composite)?;
  for (name, contents) in [
    (
      "action.yml",
      include_str!("../../../../../.github/actions/env-69-node/action.yml"),
    ),
    (
      "main.js",
      include_str!("../../../../../.github/actions/env-69-node/main.js"),
    ),
    (
      "post.js",
      include_str!("../../../../../.github/actions/env-69-node/post.js"),
    ),
  ] {
    std::fs::write(node.join(name), contents)?;
  }
  std::fs::write(
    composite.join("action.yml"),
    include_str!("../../../../../.github/actions/env-69-composite/action.yml"),
  )?;
  Ok(())
}

fn seed_node(data: &Path) -> TestResult {
  let output = std::process::Command::new("node")
    .args(["-e", "process.stdout.write(process.execPath)"])
    .output()?;
  assert!(
    output.status.success(),
    "Node is required for this acceptance test"
  );
  let path = String::from_utf8(output.stdout)?;
  let binary = node_binary_path(&node_cache_dir(data, node_version_for(20)));
  std::fs::create_dir_all(binary.parent().ok_or("node cache parent missing")?)?;
  #[cfg(unix)]
  std::os::unix::fs::symlink(path.trim(), binary)?;
  #[cfg(not(unix))]
  std::fs::copy(path.trim(), binary)?;
  Ok(())
}

#[test]
fn job_environment_node_and_nested_composite_actions_keep_scopes_and_file_updates() -> TestResult {
  let mut raw: serde_json::Value = serde_json::from_str(CAPTURE)?;
  let template = raw
    .pointer("/steps/3")
    .cloned()
    .ok_or("captured action missing")?;
  let script_template = raw
    .pointer("/steps/1")
    .cloned()
    .ok_or("captured script missing")?;
  let object = raw.as_object_mut().ok_or("captured job is not an object")?;
  object.insert(
    "environmentVariables".to_owned(),
    serde_json::from_str(LAYERS)?,
  );
  object.insert(
    "steps".to_owned(),
    serde_json::json!([
      action_step(&template, "env-69-node", "direct", "action-direct")?,
      action_step(&template, "env-69-composite", "first", "action-first")?,
      action_step(&template, "env-69-composite", "second", "action-second")?,
      script_step(&script_template)?,
    ]),
  );
  let job: AgentJobRequestMessage = serde_json::from_value(raw)?;
  let root = tempfile::tempdir()?;
  let config = RunnerConfig {
    data_dir: root.path().join("data"),
    workspace_root: root.path().join("work"),
    workspace_gc_hours: 0,
    ..RunnerConfig::default()
  };
  let workspace = config.workspace_root.join(&job.job_id);
  write_actions(&workspace)?;
  seed_node(&config.data_dir)?;
  let runner = crate::Runner::new(config, Arc::new(Mutex::new(SecretMasker::new())));
  let runtime = tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .build()?;
  let (conclusion, logs) = runtime.block_on(async {
    let cancel = CancellationToken::new();
    let mut events = runner.execute_job(job, cancel.clone());
    let result = tokio::time::timeout(Duration::from_secs(180), async {
      let mut result = None;
      let mut logs = Vec::new();
      while let Some(event) = events.recv().await {
        match event {
          RunnerEvent::JobCompleted { conclusion, .. } => result = Some(conclusion),
          RunnerEvent::Log { line, .. } => logs.push(line),
          RunnerEvent::JobStarted { .. }
          | RunnerEvent::StepStarted { .. }
          | RunnerEvent::StepCompleted { .. }
          | RunnerEvent::StepSkipped { .. }
          | RunnerEvent::LogGroup { .. }
          | RunnerEvent::Annotation { .. } => {},
        }
      }
      Ok::<_, Box<dyn Error>>((result, logs))
    })
    .await?;
    cancel.cancel();
    result
  })?;
  assert_eq!(conclusion, Some(Conclusion::Success), "{logs:?}");
  let records: Vec<serde_json::Value> =
    std::fs::read_to_string(workspace.join("env-69-node.jsonl"))?
      .lines()
      .map(serde_json::from_str)
      .collect::<Result<_, _>>()?;
  let expected = [
    ("direct", "main", "action-direct"),
    ("first-nested", "main", "action-first"),
    ("second-nested", "main", "action-second"),
    ("second-nested", "post", "action-second"),
    ("first-nested", "post", "action-first"),
    ("direct", "post", "action-direct"),
  ];
  assert_eq!(records.len(), expected.len(), "{records:?}");
  for (record, (marker, phase, shared)) in records.iter().zip(expected) {
    assert_eq!(
      record.get("marker").and_then(serde_json::Value::as_str),
      Some(marker)
    );
    assert_eq!(
      record.get("phase").and_then(serde_json::Value::as_str),
      Some(phase)
    );
    assert_eq!(
      record.get("shared").and_then(serde_json::Value::as_str),
      Some(shared)
    );
    assert_eq!(
      record.get("empty").and_then(serde_json::Value::as_str),
      Some("")
    );
    assert_eq!(
      record.get("multiline").and_then(serde_json::Value::as_str),
      Some("first\nsecond\n")
    );
    assert_eq!(
      record.get("literal").and_then(serde_json::Value::as_str),
      Some("${{ matrix.tag }}")
    );
  }
  Ok(())
}
