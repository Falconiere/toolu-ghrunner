//! Opt-in Linux production dispatch through real shell, Node, composite and post stages.
//! The Node probe is the pinned remote action the live workflow uses: a local
//! `./` action runs no `pre` (as upstream), and the probe's main and post read
//! the state its `pre` saves. A loopback API serves that pin's commit and a
//! real tarball of the probe files (byte-identical at the pin), so the
//! production download, extraction and pre/main/post dispatch all run.
#![cfg(target_os = "linux")]

use std::path::Path;
use std::sync::{Arc, Mutex};

use execution::Runner;
use shared::startup::SecretRedactor;
use shared::{
  ActionStep, AgentJobRequestMessage, Conclusion, MaskerRedactor, RunnerConfig, RunnerEvent,
  SecretMasker, TemplateToken,
};
use tokio_util::sync::CancellationToken;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const IMAGE: &str =
  "ubuntu@sha256:008173c23f95b170204355c12626cb5a965d779a7e1283b09e9cffbb1bf33ca3";

/// The live workflow's pinned Node probe (`multistep-live.yml`).
const NODE_PROBE_REF: &str = "dec29e817264994f7e8e1f49824647f739c7c063";

/// A GitHub-shaped tarball of the probe at its repository path.
fn node_probe_tarball() -> Result<Vec<u8>, Box<dyn std::error::Error>> {
  let mut archive = tar::Builder::new(flate2::write::GzEncoder::new(
    Vec::new(),
    flate2::Compression::default(),
  ));
  for (name, content) in [
    (
      "action.yml",
      include_str!("../../../.github/actions/container-node-probe/action.yml"),
    ),
    (
      "pre.js",
      include_str!("../../../.github/actions/container-node-probe/pre.js"),
    ),
    (
      "main.js",
      include_str!("../../../.github/actions/container-node-probe/main.js"),
    ),
    (
      "post.js",
      include_str!("../../../.github/actions/container-node-probe/post.js"),
    ),
  ] {
    let mut header = tar::Header::new_gnu();
    header.set_size(u64::try_from(content.len())?);
    header.set_mode(0o644);
    header.set_cksum();
    archive.append_data(
      &mut header,
      format!("Falconiere-toolu-ghrunner-dec29e8/.github/actions/container-node-probe/{name}"),
      content.as_bytes(),
    )?;
  }
  Ok(archive.into_inner()?.finish()?)
}

/// Serves the pin's commit and tarball; returns the API base URL.
async fn serve_node_probe()
-> Result<(String, tokio::task::JoinHandle<()>), Box<dyn std::error::Error>> {
  let body = node_probe_tarball()?;
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
  let api = format!("http://{}", listener.local_addr()?);
  let base = "/repos/Falconiere/toolu-ghrunner";
  let router = axum::Router::new()
    .route(
      &format!("{base}/commits/{NODE_PROBE_REF}"),
      axum::routing::get(|| async { axum::Json(serde_json::json!({ "sha": NODE_PROBE_REF })) }),
    )
    .route(
      &format!("{base}/tarball/{NODE_PROBE_REF}"),
      axum::routing::get(move || {
        let body = body.clone();
        async move { body }
      }),
    );
  let server = tokio::spawn(async move {
    let _ = axum::serve(listener, router).await;
  });
  Ok((api, server))
}

async fn write_actions(workspace: &Path) -> TestResult {
  let composite = workspace.join("composite action");
  tokio::fs::create_dir_all(&composite).await?;
  tokio::fs::write(
    composite.join("action.yml"),
    include_str!("../../../.github/actions/container-composite-probe/action.yml"),
  )
  .await?;
  Ok(())
}

fn steps() -> Vec<ActionStep> {
  let mut node = ActionStep::with_ref_type("node_probe", "repository");
  node.reference.name = Some("Falconiere/toolu-ghrunner".to_owned());
  node.reference.path = Some(".github/actions/container-node-probe".to_owned());
  node.reference.git_ref = Some(NODE_PROBE_REF.to_owned());
  node.reference.repository_type = Some("GitHub".to_owned());
  let mut composite = ActionStep::with_ref_type("composite_probe", "repository");
  composite.reference.path = Some("./composite action".to_owned());
  composite.reference.repository_type = Some("self".to_owned());
  vec![
    ActionStep::script(
      "shell",
      r#"
test "$PWD" = /github/workspace
test "$HOME" = /github/home
test "$GITHUB_WORKSPACE" = '${{ github.workspace }}'
test "$RUNNER_TEMP" = '${{ runner.temp }}'
test "$RUNNER_TOOL_CACHE" = '${{ runner.tool_cache }}'
test -f "$GITHUB_EVENT_PATH"
. /etc/os-release
test "$ID" = ubuntu
hostname > identity
printf '%s' '${{ job.container.id }}' > container-id
printf '%s' '${{ job.container.network }}' > network-id
printf 'FROM_SHELL=shell-value\n' >> "$GITHUB_ENV"
printf 'shell_result=shell-value\n' >> "$GITHUB_OUTPUT"
mkdir -p "$RUNNER_TEMP/bin"
printf '#!/bin/sh\nprintf tool-value' > "$RUNNER_TEMP/bin/container-tool"
chmod +x "$RUNNER_TEMP/bin/container-tool"
printf '%s\n' "$RUNNER_TEMP/bin" >> "$GITHUB_PATH"
"#,
      "",
    ),
    node,
    composite,
    ActionStep::script(
      "verify",
      r#"
test "$(container-tool)" = tool-value
test "$FROM_COMPOSITE" = composite-value
test '${{ steps.shell.outputs.shell_result }}' = shell-value
test '${{ steps.node_probe.outputs.node_result }}' = node-value
printf artifact-bytes > artifact
"#,
      "",
    ),
  ]
}

#[tokio::test]
#[ignore = "requires Linux, real Docker, shared test root, and Node.js download access"]
async fn job_container_real_production_shell_node_composite_and_posts() -> TestResult {
  let base = std::env::var_os("TOOLU_CONTAINER_TEST_ROOT")
    .map_or_else(std::env::temp_dir, std::path::PathBuf::from);
  let root = tempfile::Builder::new()
    .prefix("production container ")
    .tempdir_in(base)?;
  let config = RunnerConfig {
    data_dir: root.path().join("runner data"),
    workspace_root: root.path().join("work root"),
    workspace_gc_hours: 0,
    workspace: shared::WorkspaceConfig {
      mode: shared::WorkspaceMode::Fresh,
      stable_gc_hours: 0,
    },
    ..RunnerConfig::default()
  };
  let (api, server) = serve_node_probe().await?;
  let mut raw: serde_json::Value = serde_json::from_str(include_str!(
    "../../toolu-runner/tests/fixtures/job_message.json"
  ))?;
  let github = raw
    .get_mut("contextData")
    .and_then(|v| v.get_mut("github"))
    .and_then(|v| v.get_mut("d"))
    .and_then(serde_json::Value::as_array_mut)
    .ok_or("github context")?;
  let api_entry = github
    .iter_mut()
    .find(|entry| entry.get("k").and_then(serde_json::Value::as_str) == Some("api_url"))
    .ok_or("api_url context")?;
  *api_entry.get_mut("v").ok_or("api_url value")? = api.into();
  let mut job: AgentJobRequestMessage = serde_json::from_value(raw)?;
  job.job_container = Some(serde_json::from_value::<TemplateToken>(
    serde_json::json!({
      "type": 2,
      "map": [
        { "key": { "type": 0, "lit": "image" }, "value": { "type": 0, "lit": IMAGE } },
        { "key": { "type": 0, "lit": "options" }, "value": { "type": 0, "lit": "--hostname container-73-probe" } }
      ]
    }),
  )?);
  let workspace = config.workspace_root.join(&job.job_id);
  write_actions(&workspace).await?;
  job.steps = steps();
  let runner = Runner::new(config, Arc::new(Mutex::new(SecretMasker::new())));
  // Job context construction registers fixture secrets and mask hints. Events
  // stay raw inside the engine; this consumer masks before assertion output.
  let redactor = MaskerRedactor(Arc::clone(runner.masker()));
  let mut events = runner.execute_job(job, CancellationToken::new());
  let mut conclusion = None;
  let mut logs = Vec::new();
  while let Some(event) = events.recv().await {
    match event {
      RunnerEvent::JobCompleted {
        conclusion: result, ..
      } => conclusion = Some(result),
      RunnerEvent::Log { line, .. } => logs.push(redactor.redact(&line)),
      RunnerEvent::JobStarted { .. }
      | RunnerEvent::StepStarted { .. }
      | RunnerEvent::StepCompleted { .. }
      | RunnerEvent::StepSkipped { .. }
      | RunnerEvent::LogGroup { .. }
      | RunnerEvent::StepSummary { .. }
      | RunnerEvent::Annotation { .. }
      | RunnerEvent::StepMetadata { .. }
      | RunnerEvent::InfrastructureError { .. } => {},
    }
  }
  server.abort();
  assert_eq!(conclusion, Some(Conclusion::Success), "{logs:#?}");
  for stage in [
    "CONTAINER_73_NODE_PRE_OK",
    "CONTAINER_73_NODE_MAIN_OK",
    "CONTAINER_73_NODE_POST_OK",
  ] {
    assert!(
      logs.iter().any(|line| line.trim() == stage),
      "{stage}: {logs:#?}"
    );
  }
  assert_eq!(
    tokio::fs::read_to_string(workspace.join("post-marker")).await?,
    "post-value"
  );
  assert_eq!(
    tokio::fs::read_to_string(workspace.join("artifact")).await?,
    "artifact-bytes"
  );
  let docker = bollard::Docker::connect_with_local_defaults()?;
  let container = tokio::fs::read_to_string(workspace.join("container-id")).await?;
  let network = tokio::fs::read_to_string(workspace.join("network-id")).await?;
  assert!(matches!(
    docker.inspect_container(&container, None).await,
    Err(bollard::errors::Error::DockerResponseServerError {
      status_code: 404,
      ..
    })
  ));
  assert!(matches!(
    docker.inspect_network(&network, None).await,
    Err(bollard::errors::Error::DockerResponseServerError {
      status_code: 404,
      ..
    })
  ));
  Ok(())
}
