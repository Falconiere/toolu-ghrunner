//! A committed composite action runs a real shell child under a short parent deadline.

use std::collections::HashMap;
use std::error::Error;
use std::time::Duration;

use execution::execution::actions::manifest::parse_action_manifest;
use execution::execution::actions::prefetch::ActionFetcher;
use execution::execution::composite_exec::{CompositeParams, execute_composite_action};
use execution::execution::context::ExecutionContext;
use execution::execution::depth_tracker::DepthTracker;
use shared::{Conclusion, RunnerConfig, RunnerEvent};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const ACTION: &str = include_str!(concat!(
  env!("CARGO_MANIFEST_DIR"),
  "/../toolu-runner/tests/fixtures/local_actions/composite-102-bounds/action.yml"
));

#[tokio::test]
async fn parent_deadline_kills_real_composite_child_without_late_work() -> Result<(), Box<dyn Error>>
{
  let temp = tempfile::tempdir()?;
  let workspace = temp.path().join("workspace");
  std::fs::create_dir_all(&workspace)?;
  let config = RunnerConfig {
    data_dir: temp.path().join("data"),
    workspace_root: workspace.clone(),
    ..RunnerConfig::default()
  };
  let manifest = parse_action_manifest(ACTION)?;
  let inputs = HashMap::new();
  let (events, mut receiver) = mpsc::channel::<RunnerEvent>(64);
  let drain = tokio::spawn(async move { while receiver.recv().await.is_some() {} });
  let cancel = CancellationToken::new();
  let http = reqwest::Client::new();
  let fetcher = ActionFetcher::new();
  let params = CompositeParams {
    manifest: &manifest,
    step_inputs: &inputs,
    events: &events,
    workspace: &workspace,
    config: &config,
    parent_step_id: "captured-parent",
    action_dir: temp.path(),
    cancel: &cancel,
    deadline: Some(tokio::time::Instant::now() + Duration::from_millis(400)),
    http: &http,
    fetcher: &fetcher,
  };
  let mut ctx = ExecutionContext::new_for_test();
  let mut depth = DepthTracker::new();
  let result = tokio::time::timeout(
    Duration::from_secs(3),
    execute_composite_action(&params, &mut ctx, &mut depth),
  )
  .await??;
  assert_eq!(result.conclusion, Conclusion::Failure);
  assert!(workspace.join("composite-102-bounds.started").exists());
  assert!(!workspace.join("composite-102-bounds.late").exists());
  assert!(!workspace.join("composite-102-bounds.ordinary").exists());
  for file in ["composite-102-bounds.pid", "composite-102-bounds.sleep-pid"] {
    let pid = std::fs::read_to_string(workspace.join(file))?;
    let output = std::process::Command::new("ps")
      .args(["-p", pid.trim(), "-o", "stat="])
      .output()?;
    let state = String::from_utf8(output.stdout)?;
    assert!(
      !output.status.success() || state.trim().starts_with('Z'),
      "timed-out process still alive: {pid} {state}"
    );
  }
  drop(events);
  tokio::time::timeout(Duration::from_secs(1), drain).await??;
  Ok(())
}
