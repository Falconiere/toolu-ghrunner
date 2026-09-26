//! Host-independent Docker action replay checks and non-Linux rejection.

use std::sync::{Arc, Mutex};
#[cfg(target_os = "linux")]
use std::time::Duration;

use execution::Runner;
use execution::execution::action_exec::build_uses_ref;
use execution::execution::actions::resolver::resolve_action_refs;
use shared::MaskerRedactor;
use shared::startup::SecretRedactor;
use shared::{
  ActionStep, AgentJobRequestMessage, Conclusion, DictEntry, RunnerConfig, RunnerEvent,
  SecretMasker, TemplateToken,
};
use tokio_util::sync::CancellationToken;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn captured_job() -> Result<AgentJobRequestMessage, serde_json::Error> {
  serde_json::from_str(include_str!(
    "../../toolu-runner/tests/fixtures/job_container_message.json"
  ))
}

#[cfg(target_os = "linux")]
fn remote_step(id: &str, marker: &str, sha: &str) -> ActionStep {
  let mut step = ActionStep::with_ref_type(id, "repository");
  step.reference.name = Some("Falconiere/toolu-ghrunner".to_owned());
  step.reference.path = Some(".github/actions/docker-action-probe".to_owned());
  step.reference.git_ref = Some(sha.to_owned());
  step.reference.repository_type = Some("GitHub".to_owned());
  step.inputs = string_map(&[("marker", marker)]);
  step
}

#[cfg(target_os = "linux")]
async fn run_remote_job(
  config: RunnerConfig,
  job: AgentJobRequestMessage,
) -> TestResult<Vec<RunnerEvent>> {
  let runner = Runner::new(config, Arc::new(Mutex::new(SecretMasker::new())));
  let redactor = MaskerRedactor(Arc::clone(runner.masker()));
  let mut receiver = runner.execute_job(job, CancellationToken::new());
  Ok(
    tokio::time::timeout(Duration::from_secs(600), async {
      let mut events = Vec::new();
      while let Some(mut event) = receiver.recv().await {
        if let RunnerEvent::Log { line, .. } = &mut event {
          *line = redactor.redact(line);
        }
        events.push(event);
      }
      events
    })
    .await?,
  )
}

fn string_map(entries: &[(&str, &str)]) -> TemplateToken {
  TemplateToken {
    token_type: 2,
    d: Some(
      entries
        .iter()
        .map(|(key, value)| DictEntry {
          key: TemplateToken {
            token_type: 0,
            lit: Some((*key).to_owned()),
            ..TemplateToken::default()
          },
          value: TemplateToken {
            token_type: 0,
            lit: Some((*value).to_owned()),
            ..TemplateToken::default()
          },
        })
        .collect(),
    ),
    ..TemplateToken::default()
  }
}

fn registry_step() -> ActionStep {
  let mut step = ActionStep::with_ref_type("docker_registry", "containerRegistry");
  step.reference.image =
    Some("registry.example:5443/team/image@sha256:0123456789abcdef".to_owned());
  step.inputs = string_map(&[("entrypoint", "/bin/sh"), ("args", "")]);
  step
}

#[cfg(not(target_os = "linux"))]
fn rejection_step() -> ActionStep {
  let mut step = registry_step();
  step.reference.image = Some("docker://alpine:3.21".to_owned());
  step.inputs = string_map(&[
    ("entrypoint", "/bin/sh"),
    ("args", "-c 'printf forbidden > host-marker'"),
  ]);
  step
}

#[test]
fn registry_reference_keeps_full_image_and_stays_out_of_repository_prefetch() -> TestResult {
  let step = registry_step();
  assert_eq!(
    step.reference.ref_type.as_deref(),
    Some("containerRegistry")
  );
  assert_eq!(
    step.reference.image.as_deref(),
    Some("registry.example:5443/team/image@sha256:0123456789abcdef")
  );
  let uses = build_uses_ref(&step.reference);
  assert_eq!(
    uses,
    "docker://registry.example:5443/team/image@sha256:0123456789abcdef"
  );
  assert_eq!(step.input("entrypoint").as_deref(), Some("/bin/sh"));
  assert_eq!(step.input("args").as_deref(), Some(""));
  let resolved = resolve_action_refs(&[uses, "actions/checkout@v4".to_owned()])?;
  assert_eq!(resolved.len(), 1);
  assert!(resolved.contains_key("actions/checkout/v4"));
  Ok(())
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires a pushed SHA, authorized GitHub token, Linux, real Docker, and network"]
async fn remote_repository_action_runs_pre_main_post_and_reuses_image() -> TestResult {
  let sha = std::env::var("TOOLU_DOCKER_ACTIONS_REMOTE_REF")?;
  if sha.len() != 40 || !sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
    return Err("TOOLU_DOCKER_ACTIONS_REMOTE_REF must be a 40-character SHA".into());
  }
  let token = std::env::var("TOOLU_DOCKER_ACTIONS_REMOTE_TOKEN")?;
  if token.is_empty() {
    return Err("TOOLU_DOCKER_ACTIONS_REMOTE_TOKEN must not be empty".into());
  }
  let base = std::env::var_os("TOOLU_CONTAINER_TEST_ROOT")
    .map(std::path::PathBuf::from)
    .ok_or("TOOLU_CONTAINER_TEST_ROOT must be shared with Docker")?;
  let root = tempfile::Builder::new()
    .prefix("remote docker action ")
    .tempdir_in(base)?;
  let config = RunnerConfig {
    data_dir: root.path().join("runner data"),
    workspace_root: root.path().join("work root"),
    workspace_gc_hours: 0,
    ..RunnerConfig::default()
  };
  let mut job = captured_job()?;
  job.job_container = None;
  job.variables.remove("system.github.launch_endpoint");
  job
    .variables
    .get_mut("system.github.token")
    .ok_or("captured system.github.token is absent")?
    .value = token;
  job.steps = vec![
    remote_step("remote_a", "REMOTE_A", &sha),
    remote_step("remote_b", "REMOTE_B", &sha),
  ];
  let workspace = config.workspace_root.join(&job.job_id);
  let first = run_remote_job(config.clone(), job.clone()).await?;
  assert!(
    first.iter().any(|event| matches!(
      event,
      RunnerEvent::JobCompleted {
        conclusion: Conclusion::Success,
        ..
      }
    )),
    "{first:#?}"
  );
  let tag = first
    .iter()
    .find_map(|event| {
      if let RunnerEvent::Log { line, .. } = event {
        line.strip_prefix("Docker action image: ")
      } else {
        None
      }
    })
    .ok_or("prepared image identity was not logged")?;
  let docker = bollard::Docker::connect_with_local_defaults()?;
  let first_id = docker
    .inspect_image(tag)
    .await?
    .id
    .ok_or("cached image has no id")?;
  let second = run_remote_job(config.clone(), job.clone()).await?;
  assert!(
    second.iter().any(|event| matches!(
      event,
      RunnerEvent::JobCompleted {
        conclusion: Conclusion::Success,
        ..
      }
    )),
    "{second:#?}"
  );
  let second_id = docker
    .inspect_image(tag)
    .await?
    .id
    .ok_or("cached image has no id")?;
  assert_eq!(
    second_id, first_id,
    "immutable remote action rebuilt its image"
  );
  let stages = std::fs::read_to_string(workspace.join("docker-stages.txt"))?;
  for expected in [
    "REMOTE_A:pre",
    "REMOTE_A:main",
    "STATE_pre_saved=<REMOTE_A-pre>",
    "REMOTE_B:pre",
    "REMOTE_B:main",
    "REMOTE_B:post:STATE_saved=REMOTE_B-state:STATE_pre_saved=REMOTE_B-pre",
    "REMOTE_A:post:STATE_saved=REMOTE_A-state:STATE_pre_saved=REMOTE_A-pre",
  ] {
    assert_eq!(stages.matches(expected).count(), 2, "{expected}:\n{stages}");
  }
  let mut pre_failure_job = job;
  let mut pre_failure = remote_step("remote_pre_failure", "REMOTE_PRE_FAILURE", &sha);
  pre_failure.inputs = string_map(&[("marker", "REMOTE_PRE_FAILURE"), ("fail_pre", "true")]);
  pre_failure_job.steps = vec![pre_failure];
  let pre_failure_events = run_remote_job(config, pre_failure_job).await?;
  assert!(
    pre_failure_events.iter().any(|event| matches!(
      event,
      RunnerEvent::JobCompleted {
        conclusion: Conclusion::Failure,
        ..
      }
    )),
    "{pre_failure_events:#?}"
  );
  let stages = std::fs::read_to_string(workspace.join("docker-stages.txt"))?;
  assert_eq!(
    stages.matches("REMOTE_PRE_FAILURE:pre").count(),
    1,
    "{stages}"
  );
  assert_eq!(
    stages
      .matches("REMOTE_PRE_FAILURE:post:STATE_saved=<unset>:STATE_pre_saved=REMOTE_PRE_FAILURE-pre")
      .count(),
    1,
    "{stages}"
  );
  assert!(!stages.contains("REMOTE_PRE_FAILURE:main"), "{stages}");
  Ok(())
}

#[tokio::test]
async fn missing_registry_images_fail_action_resolution_before_platform_or_daemon() -> TestResult {
  for image in [
    None,
    Some(""),
    Some("   "),
    Some("docker://"),
    Some("docker://  "),
  ] {
    let root = tempfile::tempdir()?;
    let config = RunnerConfig {
      data_dir: root.path().join("runner data"),
      workspace_root: root.path().join("work root"),
      workspace_gc_hours: 0,
      ..RunnerConfig::default()
    };
    let mut job = captured_job()?;
    job.job_container = None;
    let mut step = registry_step();
    step.reference.image = image.map(str::to_owned);
    job.steps = vec![step];
    let runner = Runner::new(config, Arc::new(Mutex::new(SecretMasker::new())));
    let redactor = MaskerRedactor(Arc::clone(runner.masker()));
    let mut events = runner.execute_job(job, CancellationToken::new());
    let mut conclusion = None;
    let mut logs = Vec::new();
    while let Some(event) = events.recv().await {
      match event {
        RunnerEvent::Log { line, .. } => logs.push(redactor.redact(&line)),
        RunnerEvent::JobCompleted {
          conclusion: result, ..
        } => conclusion = Some(result),
        RunnerEvent::JobStarted { .. }
        | RunnerEvent::StepStarted { .. }
        | RunnerEvent::StepCompleted { .. }
        | RunnerEvent::StepSkipped { .. }
        | RunnerEvent::LogGroup { .. }
        | RunnerEvent::Annotation { .. } => {},
      }
    }
    assert_eq!(conclusion, Some(Conclusion::Failure), "{logs:#?}");
    assert!(
      logs
        .iter()
        .any(|line| line.contains("action manifest error: Docker action registry image is empty")),
      "missing early action-resolution diagnostic for {image:?}: {logs:#?}"
    );
  }
  Ok(())
}

#[cfg(not(target_os = "linux"))]
#[tokio::test]
async fn docker_registry_action_rejects_non_linux_without_running_host_code() -> TestResult {
  let root = tempfile::tempdir()?;
  let config = RunnerConfig {
    data_dir: root.path().join("runner data"),
    workspace_root: root.path().join("work root"),
    workspace_gc_hours: 0,
    ..RunnerConfig::default()
  };
  let mut job = captured_job()?;
  job.job_container = None;
  job.steps = vec![rejection_step()];
  let workspace = config.workspace_root.join(&job.job_id);
  let runner = Runner::new(config, Arc::new(Mutex::new(SecretMasker::new())));
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

  assert_eq!(conclusion, Some(Conclusion::Failure), "{logs:#?}");
  assert!(
    logs.iter().any(|line| {
      let lower = line.to_ascii_lowercase();
      lower.contains("docker") && lower.contains("linux")
    }),
    "missing explicit Linux-only diagnostic: {logs:#?}"
  );
  assert!(
    !workspace.join("host-marker").exists(),
    "Docker action command escaped onto the host"
  );
  Ok(())
}
