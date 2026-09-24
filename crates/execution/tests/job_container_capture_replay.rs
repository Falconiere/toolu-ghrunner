//! Captured GitHub job-container message parsing and real Linux replay.

use execution::execution::action_exec::build_uses_ref;
use shared::{AgentJobRequestMessage, TemplateToken};

#[cfg(target_os = "linux")]
use std::sync::{Arc, Mutex};
#[cfg(target_os = "linux")]
use std::time::Duration;

#[cfg(target_os = "linux")]
use execution::Runner;
#[cfg(target_os = "linux")]
use shared::startup::SecretRedactor;
#[cfg(target_os = "linux")]
use shared::{Conclusion, MaskerRedactor, RunnerConfig, RunnerEvent, SecretMasker};
#[cfg(target_os = "linux")]
use tokio_util::sync::CancellationToken;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const CAPTURE: &str = include_str!("../../toolu-runner/tests/fixtures/job_container_message.json");
const IMAGE: &str =
  "ubuntu@sha256:008173c23f95b170204355c12626cb5a965d779a7e1283b09e9cffbb1bf33ca3";

fn captured_job() -> Result<AgentJobRequestMessage, serde_json::Error> {
  serde_json::from_str(CAPTURE)
}

fn map_value<'a>(map: &'a TemplateToken, key: &str) -> Option<&'a TemplateToken> {
  map
    .d
    .as_ref()?
    .iter()
    .find_map(|entry| (entry.key.to_string_value() == Some(key)).then_some(&entry.value))
}

#[test]
fn captured_job_container_preserves_wire_shape() -> TestResult {
  let job = captured_job()?;
  assert_eq!(job.job_id, "4aa16dce-0f2f-5feb-bdd5-b2e8df68b8ef");
  assert_eq!(job.steps.len(), 6);

  let container = job
    .job_container
    .as_ref()
    .ok_or("capture has no jobContainer")?;
  assert_eq!(container.token_type, 2);
  assert_eq!(
    map_value(container, "image").and_then(TemplateToken::to_string_value),
    Some(IMAGE)
  );
  let environment = map_value(container, "env").ok_or("capture has no container env")?;
  assert_eq!(environment.token_type, 2);
  assert_eq!(
    map_value(environment, "CONTAINER_DECLARATION").and_then(TemplateToken::to_string_value),
    Some("declared-value")
  );
  let ports = map_value(container, "ports").ok_or("capture has no container ports")?;
  assert_eq!(ports.token_type, 1);
  assert_eq!(
    ports
      .seq
      .as_deref()
      .and_then(|values| values.first())
      .and_then(TemplateToken::to_string_value),
    Some("8080")
  );
  assert_eq!(
    map_value(container, "options").and_then(TemplateToken::to_string_value),
    Some("--hostname container-73-probe --cpus 1")
  );
  Ok(())
}

#[test]
fn captured_job_container_preserves_step_identities_and_action_references() -> TestResult {
  let job = captured_job()?;
  let shell = job.steps.get(1).ok_or("captured shell step is absent")?;
  assert_eq!(shell.id, "1047ca66-30a9-422d-9761-0ea97d595619");
  assert_eq!(shell.context_name.as_deref(), Some("shell"));
  let node = job.steps.get(2).ok_or("captured node step is absent")?;
  assert_eq!(node.id, "b58d48ca-3981-449b-8bea-c84892c79fe8");
  assert_eq!(node.context_name.as_deref(), Some("node_probe"));
  assert_eq!(
    build_uses_ref(&node.reference),
    "Falconiere/toolu-ghrunner/.github/actions/container-node-probe@dec29e817264994f7e8e1f49824647f739c7c063"
  );
  let composite = job
    .steps
    .get(3)
    .ok_or("captured composite step is absent")?;
  assert_eq!(composite.id, "9b992e6c-b289-49b8-a278-376b3fe1955a");
  assert_eq!(composite.context_name.as_deref(), Some("__self"));
  assert_eq!(composite.reference.repository_type.as_deref(), Some("self"));
  assert_eq!(
    composite.reference.path.as_deref(),
    Some("./.github/actions/container-composite-probe")
  );
  assert_eq!(
    build_uses_ref(&composite.reference),
    "./.github/actions/container-composite-probe"
  );
  let verify = job.steps.get(4).ok_or("captured verify step is absent")?;
  assert_eq!(verify.id, "a02941e6-31aa-4fa3-9b78-e6c964cda412");
  assert_eq!(verify.context_name.as_deref(), Some("__run"));
  Ok(())
}

#[cfg(target_os = "linux")]
fn write_composite_action(workspace: &std::path::Path) -> std::io::Result<()> {
  let action = workspace.join(".github/actions/container-composite-probe");
  std::fs::create_dir_all(&action)?;
  std::fs::write(
    action.join("action.yml"),
    include_str!("../../../.github/actions/container-composite-probe/action.yml"),
  )
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires Linux, real Docker, and network access for the captured pinned Node action"]
async fn captured_job_container_replays_shell_node_composite_and_verify() -> TestResult {
  let base = std::env::var_os("TOOLU_CONTAINER_TEST_ROOT")
    .map_or_else(std::env::temp_dir, std::path::PathBuf::from);
  let root = tempfile::Builder::new()
    .prefix("captured container replay ")
    .tempdir_in(base)?;
  let config = RunnerConfig {
    data_dir: root.path().join("runner data"),
    workspace_root: root.path().join("work root"),
    workspace_gc_hours: 0,
    ..RunnerConfig::default()
  };
  let mut job = captured_job()?;
  let workspace = config.workspace_root.join(&job.job_id);
  write_composite_action(&workspace)?;
  job.steps.remove(5);
  job.steps.remove(0);

  let runner = Runner::new(config, Arc::new(Mutex::new(SecretMasker::new())));
  // Job context construction registers the captured secrets and mask hints.
  // The engine emits raw events; mask at this consumer before assertion output.
  let redactor = MaskerRedactor(Arc::clone(runner.masker()));
  let cancel = CancellationToken::new();
  let mut events = runner.execute_job(job, cancel.clone());
  let collected = tokio::time::timeout(Duration::from_secs(180), async {
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
    (conclusion, logs)
  })
  .await;
  let (conclusion, logs) = match collected {
    Ok(result) => result,
    Err(error) => {
      cancel.cancel();
      tokio::time::timeout(Duration::from_secs(30), async {
        while events.recv().await.is_some() {}
      })
      .await?;
      return Err(error.into());
    },
  };
  assert_eq!(conclusion, Some(Conclusion::Success), "{logs:#?}");
  for stage in [
    "CONTAINER_73_SHELL_OK",
    "CONTAINER_73_NODE_PRE_OK",
    "CONTAINER_73_NODE_MAIN_OK",
    "CONTAINER_73_COMPOSITE_OK",
    "CONTAINER_73_COMMAND_FILES_OK",
    "CONTAINER_73_NODE_POST_OK",
  ] {
    assert!(
      logs.iter().any(|line| line.contains(stage)),
      "{stage}: {logs:#?}"
    );
  }
  assert_eq!(
    std::fs::read_to_string(workspace.join("container-73.txt"))?,
    "container-73-artifact\n"
  );
  assert_eq!(
    std::fs::read_to_string(workspace.join("post-marker"))?,
    "post-value"
  );

  let identities = std::fs::read_to_string(workspace.join("container-73-runtime.txt"))?;
  let mut ids = identities.lines();
  let container = ids
    .next()
    .ok_or("captured replay did not record a container id")?;
  let network = ids
    .next()
    .ok_or("captured replay did not record a network id")?;
  assert!(
    ids.next().is_none(),
    "unexpected container identity data: {identities}"
  );
  let docker = bollard::Docker::connect_with_local_defaults()?;
  assert!(matches!(
    docker.inspect_container(container, None).await,
    Err(bollard::errors::Error::DockerResponseServerError {
      status_code: 404,
      ..
    })
  ));
  assert!(matches!(
    docker.inspect_network(network, None).await,
    Err(bollard::errors::Error::DockerResponseServerError {
      status_code: 404,
      ..
    })
  ));
  Ok(())
}
