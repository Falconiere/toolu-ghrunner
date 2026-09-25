//! Opt-in Linux production dispatch through real shell, Node, composite and post stages.
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

fn write_actions(workspace: &Path) -> TestResult {
  let node = workspace.join("node action");
  let composite = workspace.join("composite action");
  std::fs::create_dir_all(&node)?;
  std::fs::create_dir_all(&composite)?;
  for (name, source) in [
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
    std::fs::write(node.join(name), source)?;
  }
  std::fs::write(
    composite.join("action.yml"),
    include_str!("../../../.github/actions/container-composite-probe/action.yml"),
  )?;
  Ok(())
}

fn steps() -> Vec<ActionStep> {
  let mut node = ActionStep::with_ref_type("node_probe", "repository");
  node.reference.path = Some("./node action".to_owned());
  node.reference.repository_type = Some("self".to_owned());
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
    ..RunnerConfig::default()
  };
  let mut job: AgentJobRequestMessage = serde_json::from_str(include_str!(
    "../../toolu-runner/tests/fixtures/job_message.json"
  ))?;
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
  write_actions(&workspace)?;
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
      | RunnerEvent::Annotation { .. } => {},
    }
  }
  assert_eq!(conclusion, Some(Conclusion::Success), "{logs:#?}");
  assert_eq!(
    std::fs::read_to_string(workspace.join("post-marker"))?,
    "post-value"
  );
  assert_eq!(
    std::fs::read_to_string(workspace.join("artifact"))?,
    "artifact-bytes"
  );
  let docker = bollard::Docker::connect_with_local_defaults()?;
  let container = std::fs::read_to_string(workspace.join("container-id"))?;
  let network = std::fs::read_to_string(workspace.join("network-id"))?;
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
