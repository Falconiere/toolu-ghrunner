//! Captured-message variants exercising input handoff to a real Node process.
//!
//! Retain the acquired parent action step/context/with tokens, but supply a
//! committed Node probe manifest at its local path. Boundary variants replace
//! the captured string input or corrupt its expression; these are documented
//! transformations, not additional GitHub captures. Node on PATH is required.

use std::error::Error;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use execution::Runner;
use execution::node::runtime::{node_binary_path, node_cache_dir, node_version_for};
use shared::{AgentJobRequestMessage, Conclusion, RunnerConfig, RunnerEvent, SecretMasker};
use tokio_util::sync::CancellationToken;

type TestResult = Result<(), Box<dyn Error>>;
const CAPTURE: &str = include_str!("incoming_contexts_matrix_0.json");

#[tokio::test]
async fn acquired_with_tokens_reach_node_once_and_selected_defaults_resolve() -> TestResult {
  for value in ["world", "${{ matrix.tag }}"] {
    let dir = tempfile::tempdir()?;
    let msg = node_variant(value, false)?;
    let cfg = config(dir.path());
    let workspace = cfg.workspace_root.join(&msg.job_id);
    seed_probe(&cfg.data_dir, &workspace)?;
    let (result, logs) = execute(msg, cfg).await?;
    assert_eq!(result, Some(Conclusion::Success), "{logs:?}");
    let observed: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(
      workspace.join("node-inputs.json"),
    )?)?;
    assert_eq!(
      observed,
      serde_json::json!({"who":value,"expected":value,"fallback":value})
    );
  }
  Ok(())
}

#[tokio::test]
async fn malformed_acquired_with_expression_fails_before_node_runs() -> TestResult {
  let dir = tempfile::tempdir()?;
  let msg = node_variant("world", true)?;
  let cfg = config(dir.path());
  let workspace = cfg.workspace_root.join(&msg.job_id);
  seed_probe(&cfg.data_dir, &workspace)?;
  let (result, logs) = execute(msg, cfg).await?;
  assert_eq!(result, Some(Conclusion::Failure), "{logs:?}");
  assert!(!workspace.join("node-inputs.json").exists());
  assert!(
    logs.iter().any(|line| line.contains("##[error]")),
    "{logs:?}"
  );
  Ok(())
}

fn config(root: &Path) -> RunnerConfig {
  RunnerConfig {
    data_dir: root.join("data"),
    workspace_root: root.join("work"),
    workspace_gc_hours: 0,
    ..RunnerConfig::default()
  }
}

fn node_variant(value: &str, malformed: bool) -> Result<AgentJobRequestMessage, Box<dyn Error>> {
  let mut msg: AgentJobRequestMessage = serde_json::from_str(CAPTURE)?;
  let inputs = msg
    .context_data
    .get_mut("inputs")
    .and_then(|data| data.d.as_mut())
    .ok_or("captured inputs missing")?;
  let who = inputs
    .iter_mut()
    .find(|entry| entry.key.s.as_deref() == Some("who"))
    .ok_or("captured who missing")?;
  who.value.s = Some(value.to_owned());
  msg.steps.retain(|step| {
    step
      .inputs
      .to_map()
      .get("who")
      .is_some_and(|token| token.expr.as_deref() == Some("inputs.who"))
  });
  assert_eq!(
    msg.steps.len(),
    1,
    "retain the acquired expression-bearing action only"
  );
  let step = msg.steps.first_mut().ok_or("captured action missing")?;
  for entry in step.inputs.d.as_mut().ok_or("with entries missing")? {
    if entry.key.lit.as_deref() == Some("expected") {
      entry.value.lit = Some(value.to_owned());
    }
    if malformed && entry.key.lit.as_deref() == Some("who") {
      entry.value.expr = Some("inputs[".to_owned());
    }
  }
  Ok(msg)
}

fn seed_probe(data: &Path, workspace: &Path) -> TestResult {
  let action = workspace.join(".github/actions/context-68-parent");
  std::fs::create_dir_all(&action)?;
  std::fs::write(
    action.join("action.yml"),
    include_str!("incoming_contexts_node_action.yml"),
  )?;
  std::fs::write(
    action.join("main.js"),
    include_str!("incoming_contexts_node_action.js"),
  )?;
  // Resolve shims to a real executable. Runtime downloading/version selection
  // is not under test; the installed real Node runs the probe without network.
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
  std::os::unix::fs::symlink(path.trim(), &binary)?;
  #[cfg(not(unix))]
  std::fs::copy(path.trim(), &binary)?;
  Ok(())
}

async fn execute(
  msg: AgentJobRequestMessage,
  cfg: RunnerConfig,
) -> Result<(Option<Conclusion>, Vec<String>), Box<dyn Error>> {
  let runner = Runner::new(cfg, Arc::new(Mutex::new(SecretMasker::new())));
  let cancel = CancellationToken::new();
  let mut events = runner.execute_job(msg, cancel.clone());
  let observed = tokio::time::timeout(Duration::from_secs(30), async {
    let mut result = None;
    let mut logs = Vec::new();
    while let Some(event) = events.recv().await {
      match event {
        RunnerEvent::Log { line, .. } => logs.push(line),
        RunnerEvent::JobCompleted { conclusion, .. } => result = Some(conclusion),
        RunnerEvent::JobStarted { .. }
        | RunnerEvent::StepStarted { .. }
        | RunnerEvent::StepCompleted { .. }
        | RunnerEvent::StepSkipped { .. }
        | RunnerEvent::LogGroup { .. }
        | RunnerEvent::Annotation { .. } => {},
      }
    }
    (result, logs)
  })
  .await;
  cancel.cancel();
  Ok(observed?)
}
