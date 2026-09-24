//! Real-daemon composite shell routing with host-readable file commands.

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex};

use execution::docker::container_spec::ContainerSpec;
use execution::docker::job_container::JobContainer;
use execution::execution::actions::manifest::parse_action_manifest;
use execution::execution::actions::prefetch::ActionFetcher;
use execution::execution::composite_exec::{CompositeParams, execute_composite_action};
use execution::execution::context::ExecutionContext;
use execution::execution::depth_tracker::DepthTracker;
use futures_util::FutureExt;
use shared::{Conclusion, RunnerConfig, SecretMasker};
use tokio_util::sync::CancellationToken;

#[tokio::test]
#[ignore = "requires real Docker with a shared TOOLU_CONTAINER_TEST_ROOT"]
async fn job_container_real_composite_uses_container_shell_and_paths()
-> Result<(), Box<dyn std::error::Error>> {
  let base = std::env::var_os("TOOLU_CONTAINER_TEST_ROOT")
    .map_or_else(std::env::temp_dir, std::path::PathBuf::from);
  let root = tempfile::Builder::new()
    .prefix("composite container ")
    .tempdir_in(base)?;
  let config = RunnerConfig {
    data_dir: root.path().join("runner data"),
    workspace_root: root.path().join("work"),
    ..RunnerConfig::default()
  };
  let workspace = config.workspace_root.join("job");
  let masker = Arc::new(Mutex::new(SecretMasker::new()));
  let mut ctx = ExecutionContext::with_masker(Arc::clone(&masker));
  ctx.set_workspace(Some(workspace.clone()));
  ctx.set_runner_context("container-probe", &config.data_dir)?;
  let spec = ContainerSpec {
    image: "ubuntu@sha256:008173c23f95b170204355c12626cb5a965d779a7e1283b09e9cffbb1bf33ca3"
      .to_owned(),
    credentials: None,
    env: HashMap::new(),
    ports: Vec::new(),
    volumes: Vec::new(),
    options: vec!["--hostname".to_owned(), "composite-probe".to_owned()],
  };
  let cancel = CancellationToken::new();
  let container = Arc::new(
    JobContainer::start(&spec, &config, &workspace, masker, &cancel)
      .await?
      .ok_or("container setup unexpectedly cancelled")?,
  );
  ctx.set_job_container(Arc::clone(&container));
  let result = AssertUnwindSafe(async {
    let manifest = parse_action_manifest(
      r#"
name: Container composite probe
description: Runs a real shell and writes real file commands.
runs:
  using: composite
  steps:
    - shell: sh
      env:
        EXPECTED_ACTION: ${{ github.action_path }}
      run: |
        test "$(hostname)" = composite-probe
        test "$PWD" = /github/workspace
        test "$RUNNER_TEMP" = '${{ runner.temp }}'
        test "$GITHUB_WORKSPACE" = '${{ github.workspace }}'
        test "$EXPECTED_ACTION" = "$GITHUB_ACTION_PATH"
        test -d '${{ github.action_path }}'
        printf 'FROM_COMPOSITE=works\n' >> "$GITHUB_ENV"
        printf '/opt/composite-tools\n' >> "$GITHUB_PATH"
        printf 'container\n' > marker
"#,
    )?;
    let (events, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move {
      let mut logs = Vec::new();
      while let Some(event) = rx.recv().await {
        logs.push(event);
      }
      logs
    });
    let inputs = HashMap::new();
    let fetcher = ActionFetcher::new();
    // One client for this isolated test's entire composite invocation. This
    // shell-only manifest makes no HTTP requests; no cross-test pool is needed.
    let http = reqwest::Client::new();
    let params = CompositeParams {
      manifest: &manifest,
      step_inputs: &inputs,
      events: &events,
      workspace: &workspace,
      config: &config,
      parent_step_id: "composite",
      action_dir: &workspace,
      cancel: &cancel,
      deadline: None,
      http: &http,
      fetcher: &fetcher,
    };
    let output = execute_composite_action(&params, &mut ctx, &mut DepthTracker::new()).await?;
    drop(events);
    let logs = drain.await?;
    assert_eq!(output.conclusion, Conclusion::Success, "{logs:?}");
    assert_eq!(
      output
        .env_additions
        .get("FROM_COMPOSITE")
        .map(String::as_str),
      Some("works")
    );
    assert_eq!(output.path_additions, vec!["/opt/composite-tools"]);
    assert_eq!(
      std::fs::read_to_string(workspace.join("marker"))?,
      "container\n"
    );
    Ok::<(), Box<dyn std::error::Error>>(())
  })
  .catch_unwind()
  .await;
  let cleanup = container.cleanup().await;
  result.map_err(|_panic| "composite assertion failed after owned resource cleanup")??;
  cleanup?;
  Ok(())
}
