//! Opt-in production Docker-action replay against a real Linux daemon.
#![cfg(target_os = "linux")]

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bollard::query_parameters::{CreateImageOptions, ListContainersOptions};
use execution::Runner;
use futures_util::StreamExt;
use shared::startup::SecretRedactor;
use shared::{
  ActionStep, AgentJobRequestMessage, AnnotationLevel, Conclusion, DictEntry, MaskerRedactor,
  RunnerConfig, RunnerEvent, SecretMasker, TemplateToken,
};
use tokio_util::sync::CancellationToken;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const CAPTURE: &str = include_str!("../../toolu-runner/tests/fixtures/job_container_message.json");
const PROBE: &str = concat!(
  env!("CARGO_MANIFEST_DIR"),
  "/../../.github/actions/docker-action-probe"
);
const ALPINE: &str =
  "alpine@sha256:ce64758a109eb420d874a118f87920e625e12d3634e03b4a5573fd9f6e5d3507";

fn literal(value: &str) -> TemplateToken {
  TemplateToken {
    token_type: 0,
    lit: Some(value.to_owned()),
    ..TemplateToken::default()
  }
}

fn token_map(entries: &[(&str, &str)]) -> TemplateToken {
  TemplateToken {
    token_type: 2,
    d: Some(
      entries
        .iter()
        .map(|(key, value)| DictEntry {
          key: literal(key),
          value: literal(value),
        })
        .collect(),
    ),
    ..TemplateToken::default()
  }
}

fn captured_job() -> TestResult<AgentJobRequestMessage> {
  let mut job: AgentJobRequestMessage = serde_json::from_str(CAPTURE)?;
  job.job_container = None;
  Ok(job)
}

fn service_token() -> TestResult<TemplateToken> {
  Ok(serde_json::from_value(serde_json::json!({"type":2,"map":[
    {"key":{"type":0,"lit":"web"},"value":{"type":2,"map":[
      {"key":{"type":0,"lit":"image"},"value":{"type":0,"lit":"nginx@sha256:65645c7bb6a0661892a8b03b89d0743208a18dd2f3f17a54ef4b76fb8e2f2a10"}},
      {"key":{"type":0,"lit":"ports"},"value":{"type":1,"seq":[{"type":0,"lit":"80"}]}},
      {"key":{"type":0,"lit":"options"},"value":{"type":0,"lit":"--health-cmd 'wget -q -O /dev/null http://localhost' --health-interval 1s --health-retries 3"}}
    ]}}
  ]}))?)
}

fn local_step(id: &str, path: &str, inputs: &[(&str, &str)]) -> ActionStep {
  let mut step = ActionStep::with_ref_type(id, "repository");
  step.reference.repository_type = Some("self".to_owned());
  step.reference.path = Some(path.to_owned());
  step.inputs = token_map(inputs);
  step
}

fn registry_step(id: &str, entrypoint: &str, args: &str) -> ActionStep {
  let mut step = ActionStep::with_ref_type(id, "containerRegistry");
  step.reference.image = Some(format!("docker://{ALPINE}"));
  step.inputs = token_map(&[("entrypoint", entrypoint), ("args", args)]);
  step
}

fn linux_root() -> TestResult<tempfile::TempDir> {
  let base = std::env::var_os("TOOLU_CONTAINER_TEST_ROOT")
    .map(PathBuf::from)
    .ok_or("TOOLU_CONTAINER_TEST_ROOT must be shared with the Docker daemon")?;
  Ok(
    tempfile::Builder::new()
      .prefix("docker action ")
      .tempdir_in(base)?,
  )
}

fn config_for(root: &Path) -> RunnerConfig {
  RunnerConfig {
    data_dir: root.join("runner data"),
    workspace_root: root.join("work root"),
    workspace_gc_hours: 0,
    ..RunnerConfig::default()
  }
}

async fn seed_probe(workspace: &Path, name: &str, manifest: &str) -> TestResult {
  let source = Path::new(PROBE);
  let target = workspace.join(".github/actions").join(name);
  tokio::fs::create_dir_all(&target).await?;
  for file in [
    "Dockerfile",
    "pre.sh",
    "main.sh",
    "post.sh",
    "docker-probe-tool",
  ] {
    tokio::fs::copy(source.join(file), target.join(file)).await?;
  }
  tokio::fs::copy(source.join(manifest), target.join("action.yml")).await?;
  Ok(())
}

async fn collect_job(
  config: RunnerConfig,
  job: AgentJobRequestMessage,
  cancel: CancellationToken,
) -> TestResult<Vec<RunnerEvent>> {
  let runner = Runner::new(config, Arc::new(Mutex::new(SecretMasker::new())));
  let redactor = MaskerRedactor(Arc::clone(runner.masker()));
  let scenario_steps = job
    .steps
    .iter()
    .map(|step| step.id.as_str())
    .collect::<Vec<_>>()
    .join(", ");
  let mut receiver = runner.execute_job(job, cancel);
  Ok(
    tokio::time::timeout(Duration::from_secs(300), async {
      let mut events = Vec::new();
      while let Some(mut event) = receiver.recv().await {
        mask_event(&redactor, &mut event);
        events.push(event);
      }
      events
    })
    .await
    .map_err(|error| {
      std::io::Error::other(format!(
        "Docker replay timed out for steps [{scenario_steps}]: {error}"
      ))
    })?,
  )
}

fn mask_event(redactor: &MaskerRedactor, event: &mut RunnerEvent) {
  if let RunnerEvent::Log { line, .. } = event {
    *line = redactor.redact(line);
  }
}

fn job_conclusion(events: &[RunnerEvent]) -> Option<Conclusion> {
  events.iter().find_map(|event| match event {
    RunnerEvent::JobCompleted { conclusion, .. } => Some(*conclusion),
    RunnerEvent::JobStarted { .. }
    | RunnerEvent::StepStarted { .. }
    | RunnerEvent::StepCompleted { .. }
    | RunnerEvent::StepSkipped { .. }
    | RunnerEvent::Log { .. }
    | RunnerEvent::LogGroup { .. }
    | RunnerEvent::Annotation { .. } => None,
  })
}

fn is_missing<T>(result: &Result<T, bollard::errors::Error>) -> bool {
  matches!(
    result,
    Err(bollard::errors::Error::DockerResponseServerError {
      status_code,
      ..
    }) if *status_code == 404
  )
}

async fn action_container_ids(
  docker: &bollard::Docker,
  step_id: &str,
) -> TestResult<HashSet<String>> {
  let filters = HashMap::from([(
    "label".to_owned(),
    vec![format!("io.toolu.action-step={step_id}")],
  )]);
  let containers = docker
    .list_containers(Some(ListContainersOptions {
      all: true,
      filters: Some(filters),
      ..Default::default()
    }))
    .await?;
  Ok(containers.into_iter().filter_map(|item| item.id).collect())
}

async fn wait_for_file(path: &Path) -> TestResult {
  tokio::time::timeout(Duration::from_secs(180), async {
    while !path.exists() {
      tokio::time::sleep(Duration::from_millis(100)).await;
    }
  })
  .await?;
  Ok(())
}

async fn pull_alpine(docker: &bollard::Docker) -> TestResult {
  let mut stream = docker.create_image(
    Some(CreateImageOptions {
      from_image: Some(ALPINE.to_owned()),
      ..Default::default()
    }),
    None,
    None,
  );
  while let Some(item) = stream.next().await {
    item?;
  }
  Ok(())
}

#[tokio::test]
#[ignore = "requires Linux, real Docker, and a daemon-shared test root"]
async fn local_actions_preserve_argv_env_commands_state_and_lifo_posts() -> TestResult {
  let root = linux_root()?;
  let config = config_for(root.path());
  let mut job = captured_job()?;
  let workspace = config.workspace_root.join(&job.job_id);
  seed_probe(&workspace, "probe-a", "action.yml").await?;
  seed_probe(&workspace, "probe-b", "action.yml").await?;
  seed_probe(&workspace, "probe-c", "action-false-stages.yml").await?;

  let mut first = local_step("docker_a", "./.github/actions/probe-a", &[("marker", "A")]);
  first.environment = Some(token_map(&[("OVERRIDE_ME", "step-override")]));
  let mut second = local_step(
    "docker_b",
    "./.github/actions/probe-b",
    &[("marker", "B"), ("fail_post", "true")],
  );
  second.environment = Some(token_map(&[("OVERRIDE_ME", "step-override")]));
  let third = local_step("docker_c", "./.github/actions/probe-c", &[("marker", "C")]);
  job.steps = vec![
    first,
    second,
    third,
    ActionStep::script(
      "verify",
      r#"
test "$FROM_DOCKER" = C-env
test '${{ steps.docker_a.outputs.probe_output }}' = A-output
test '${{ steps.docker_b.outputs.probe_output }}' = B-output
test '${{ steps.docker_c.outputs.probe_output }}' = C-output
test "$(docker-probe-tool)" = docker-probe-tool-ok
test "$(cat docker-summary-A.txt)" = summary-A
test "$(cat docker-summary-B.txt)" = summary-B
test "$(cat docker-summary-C.txt)" = summary-C
printf verified > docker-verify
"#,
      "",
    ),
  ];

  let events = collect_job(config, job, CancellationToken::new()).await?;
  assert_eq!(
    job_conclusion(&events),
    Some(Conclusion::Failure),
    "{events:#?}"
  );
  assert_eq!(
    tokio::fs::read_to_string(workspace.join("docker-verify")).await?,
    // The verify script uses `printf verified`, which writes no newline.
    "verified"
  );
  assert_eq!(
    tokio::fs::read_to_string(workspace.join("docker-summary-B.txt")).await?,
    "summary-B\n"
  );
  let stages = tokio::fs::read_to_string(workspace.join("docker-stages.txt")).await?;
  for expected in [
    "A:main",
    "argc=3",
    "arg0=<>\narg1=<two words>\narg2=<A>",
    "DEFAULT_ONLY=<manifest-default>",
    "OVERRIDE_ME=<step-override>",
    "STATE_pre_saved=<<unset>>",
    "B:main",
    "arg0=<>\narg1=<two words>\narg2=<B>",
    "C:main",
    "arg0=<>\narg1=<two words>\narg2=<C>",
    "B:post:STATE_saved=B-state:STATE_pre_saved=<unset>",
    "A:post:STATE_saved=A-state:STATE_pre_saved=<unset>",
  ] {
    assert!(stages.contains(expected), "missing {expected:?}:\n{stages}");
  }
  assert!(!stages.contains("A:pre"), "local pre stage ran:\n{stages}");
  assert!(!stages.contains("B:pre"), "local pre stage ran:\n{stages}");
  assert!(!stages.contains("C:pre"), "false pre-if ran:\n{stages}");
  assert!(!stages.contains("C:post"), "false post-if ran:\n{stages}");
  let b_post = stages.find("B:post").ok_or("B post absent")?;
  let a_post = stages.find("A:post").ok_or("A post absent")?;
  assert!(b_post < a_post, "posts did not drain LIFO:\n{stages}");
  let annotations: Vec<_> = events
    .iter()
    .filter_map(|event| match event {
      RunnerEvent::Annotation {
        level,
        message,
        title,
        ..
      } if *level == AnnotationLevel::Warning => Some((message.as_str(), title.as_deref())),
      RunnerEvent::JobStarted { .. }
      | RunnerEvent::JobCompleted { .. }
      | RunnerEvent::StepStarted { .. }
      | RunnerEvent::StepCompleted { .. }
      | RunnerEvent::StepSkipped { .. }
      | RunnerEvent::Log { .. }
      | RunnerEvent::LogGroup { .. }
      | RunnerEvent::Annotation { .. } => None,
    })
    .collect();
  assert_eq!(
    annotations,
    [
      ("annotation-A", Some("Docker probe")),
      ("annotation-B", Some("Docker probe")),
      ("annotation-C", Some("Docker probe")),
    ]
  );
  Ok(())
}

#[tokio::test]
#[ignore = "requires Linux, real Docker, and a daemon-shared test root"]
async fn registry_and_manifest_arg_variants_execute_exact_argv() -> TestResult {
  let root = linux_root()?;
  let config = config_for(root.path());
  let mut job = captured_job()?;
  let workspace = config.workspace_root.join(&job.job_id);
  seed_probe(&workspace, "absent", "action-absent-args.yml").await?;
  seed_probe(&workspace, "empty", "action-empty-args.yml").await?;
  seed_probe(
    &workspace,
    "composite-registry",
    "action-composite-registry.yml",
  )
  .await?;
  job.steps = vec![
    local_step(
      "absent_args",
      "./.github/actions/absent",
      &[("marker", "ABSENT"), ("args", "one '' 'two words'")],
    ),
    local_step(
      "empty_args",
      "./.github/actions/empty",
      &[("marker", "EMPTY"), ("args", "forbidden")],
    ),
    registry_step(
      "registry",
      "/bin/sh",
      "-c 'printf registry-ok > \"$GITHUB_WORKSPACE/registry-marker\"'",
    ),
    local_step(
      "composite_registry",
      "./.github/actions/composite-registry",
      &[],
    ),
    ActionStep::script(
      "verify_composite_registry",
      r#"
test '${{ steps.composite_registry.outputs.marker }}' = composite-output
test "$(cat composite-registry-marker)" = composite-registry-ok
printf composite-verified > composite-registry-verified
"#,
      "",
    ),
  ];

  let events = collect_job(config, job, CancellationToken::new()).await?;
  assert_eq!(
    job_conclusion(&events),
    Some(Conclusion::Success),
    "{events:#?}"
  );
  assert_eq!(
    tokio::fs::read_to_string(workspace.join("registry-marker")).await?,
    "registry-ok"
  );
  assert_eq!(
    tokio::fs::read_to_string(workspace.join("composite-registry-marker")).await?,
    "composite-registry-ok"
  );
  assert_eq!(
    tokio::fs::read_to_string(workspace.join("composite-registry-verified")).await?,
    "composite-verified"
  );
  let stages = tokio::fs::read_to_string(workspace.join("docker-stages.txt")).await?;
  assert!(
    stages.contains("ABSENT:main\nargc=3\narg0=<one>\narg1=<>\narg2=<two words>"),
    "{stages}"
  );
  assert!(stages.contains("EMPTY:main\nargc=0"), "{stages}");
  Ok(())
}

#[path = "../src/execution/tests/docker_action_lifecycle.rs"]
mod lifecycle;
