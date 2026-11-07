//! `uses: $/x` inside a composite action goes through the real nested-step
//! path (`execute_composite_action` -> `run_nested_uses_step`): with the
//! `actions_self_repository` flag it resolves against the workflow repository
//! and SHA (the fetch afterwards is pointed at a closed loopback proxy and fails);
//! without the flag it is refused with upstream's message.

use std::collections::HashMap;

use execution::execution::actions::manifest::{
  ActionDefinition, ActionRuns, CompositeStep, RunsUsing,
};
use execution::execution::actions::prefetch::ActionFetcher;
use execution::execution::composite_exec::{CompositeParams, execute_composite_action};
use execution::execution::context::ExecutionContext;
use execution::execution::depth_tracker::DepthTracker;
use shared::{RunnerConfig, RunnerEvent};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

fn manifest() -> ActionDefinition {
  ActionDefinition {
    name: "self".to_owned(),
    description: String::new(),
    inputs: HashMap::new(),
    outputs: HashMap::new(),
    runs: ActionRuns {
      args: None,
      env: HashMap::new(),
      entrypoint: None,
      pre_entrypoint: None,
      post_entrypoint: None,
      using: RunsUsing::Composite,
      main: None,
      pre: None,
      post: None,
      pre_if: None,
      post_if: None,
      image: None,
      steps: vec![CompositeStep {
        id: Some("child".to_owned()),
        name: None,
        run: None,
        shell: None,
        working_directory: None,
        env: HashMap::new(),
        condition: None,
        uses: Some("$/actions/child".to_owned()),
        continue_on_error: None,
        with: HashMap::new(),
      }],
    },
  }
}

/// Run the composite; return the error text and every log line.
async fn run(enabled: bool) -> Result<(String, Vec<String>), Box<dyn std::error::Error>> {
  let dir = tempfile::tempdir()?;
  let workspace = dir.path().join("_work");
  std::fs::create_dir_all(&workspace)?;
  let config = RunnerConfig {
    data_dir: dir.path().to_path_buf(),
    workspace_root: workspace.clone(),
    ..RunnerConfig::default()
  };
  let (events, mut rx) = mpsc::channel(256);
  let manifest = manifest();
  let inputs = HashMap::new();
  let cancel = CancellationToken::new();
  // Hermetic: every request goes to a closed loopback port, so the fetch
  // after resolution fails immediately and nothing reaches the network.
  let http = reqwest::Client::builder()
    .proxy(reqwest::Proxy::all("http://127.0.0.1:1")?)
    .timeout(std::time::Duration::from_secs(5))
    .build()?;
  let fetcher = ActionFetcher::new(&shared::NetworkConfig::default())?;
  let params = CompositeParams {
    manifest: &manifest,
    step_inputs: &inputs,
    events: &events,
    workspace: &workspace,
    config: &config,
    parent_step_id: "parent",
    action_dir: dir.path(),
    cancel: &cancel,
    deadline: None,
    http: &http,
    fetcher: &fetcher,
  };
  let mut ctx = ExecutionContext::new_for_test();
  ctx.set_runner_context("test-runner", dir.path())?;
  if enabled {
    ctx.enable_self_repository(Some(("octo/workflow-repo".to_owned(), SHA.to_owned())));
  }
  let mut depth = DepthTracker::new();
  let result = execute_composite_action(&params, &mut ctx, &mut depth).await;
  drop(events);
  let mut lines = Vec::new();
  while let Some(event) = rx.recv().await {
    if let RunnerEvent::Log { line, .. } = event {
      lines.push(line);
    }
  }
  let error = result.err().map(|e| e.to_string()).unwrap_or_default();
  Ok((error, lines))
}

#[tokio::test]
async fn flag_on_resolves_to_the_workflow_repository_and_sha()
-> Result<(), Box<dyn std::error::Error>> {
  let (error, lines) = run(true).await?;
  let resolved = format!("octo/workflow-repo/actions/child@{SHA}");
  assert!(
    lines
      .iter()
      .any(|l| l.contains(&format!("Resolving {resolved}"))),
    "expected the action header log `Resolving {resolved}` (emitted only after resolution) in {lines:?}; error {error:?}"
  );
  Ok(())
}

#[tokio::test]
async fn flag_off_refuses_the_reference() -> Result<(), Box<dyn std::error::Error>> {
  let (error, lines) = run(false).await?;
  let all = format!("{error} {}", lines.join(" "));
  assert!(
    all.contains("Unable to resolve self-reference"),
    "got: {all}"
  );
  Ok(())
}
