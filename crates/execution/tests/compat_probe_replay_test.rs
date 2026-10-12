//! Issue #103 S2: the captured reference-lane `docker` job replayed through
//! the production engine against a real Linux Docker daemon.
//!
//! `compat_103_docker_message.json` is the sanitized acquisition of the
//! toolu-lane `probe / docker` job from run 38301583727. Its container step
//! (`uses: docker://alpine@sha256:d9e8…`) writes a marker and step outputs that
//! only exist inside Alpine (`/etc/alpine-release`, 3.20.10) plus the `who`
//! input it received; the captured verify step asserts all three. Replacing the
//! container step with a script that merely echoes a `docker://` string, or
//! emptying `inputs.who`, must make that verify step fail.
//!
//! Opt-in (real Docker): run explicitly, as the `ci.yml` Docker step does —
//! `TOOLU_CONTAINER_TEST_ROOT=<daemon-shared dir> cargo test -p execution
//! --test compat_probe_replay_test -- --ignored --test-threads=1`.
#![cfg(target_os = "linux")]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use execution::Runner;
use shared::{AgentJobRequestMessage, Conclusion, RunnerConfig, RunnerEvent, SecretMasker};
use tokio_util::sync::CancellationToken;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const CAPTURE: &str = include_str!("compat_103_docker_message.json");
const DIGEST: &str = "sha256:d9e853e87e55526f6b2917df91a2115c36dd7c696a35be12163d44e6e2a4b6bc";

struct Replayed {
  conclusion: Conclusion,
  failed: Vec<String>,
  outputs: HashMap<String, String>,
  marker: Option<String>,
}

fn raw() -> TestResult<serde_json::Value> {
  Ok(serde_json::from_str(CAPTURE)?)
}

fn set(value: &mut serde_json::Value, pointer: &str, new: serde_json::Value) -> TestResult {
  *value
    .pointer_mut(pointer)
    .ok_or_else(|| format!("captured {pointer} missing"))? = new;
  Ok(())
}

fn probe_containers() -> TestResult<usize> {
  let out = std::process::Command::new("docker")
    .args(["ps", "-a", "--no-trunc", "--format", "{{.Image}}"])
    .output()?;
  assert!(out.status.success(), "docker ps failed: {out:?}");
  Ok(
    String::from_utf8(out.stdout)?
      .lines()
      .filter(|image| image.contains(DIGEST))
      .count(),
  )
}

fn replay(raw: serde_json::Value) -> TestResult<Replayed> {
  let message: AgentJobRequestMessage = serde_json::from_value(raw)?;
  let names: HashMap<String, String> = message
    .steps
    .iter()
    .map(|step| {
      (
        step.id.clone(),
        step.context_name.clone().unwrap_or_default(),
      )
    })
    .collect();
  let base = std::env::var_os("TOOLU_CONTAINER_TEST_ROOT")
    .map(PathBuf::from)
    .ok_or("TOOLU_CONTAINER_TEST_ROOT must be shared with the Docker daemon")?;
  let root = tempfile::Builder::new()
    .prefix("compat 103 ")
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
  let workspace = config.workspace_root.join(&message.job_id);
  let runner = Runner::new(config, Arc::new(Mutex::new(SecretMasker::new())));
  let runtime = tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .build()?;
  let (failed, completed) = runtime.block_on(async {
    let cancel = CancellationToken::new();
    let mut events = runner.execute_job(message, cancel.clone());
    let collected = tokio::time::timeout(Duration::from_secs(300), async {
      let mut failed = Vec::new();
      let mut completed = None;
      while let Some(event) = events.recv().await {
        match event {
          RunnerEvent::StepCompleted {
            step_id,
            conclusion: Conclusion::Failure,
            ..
          } => failed.push(names.get(&step_id).cloned().unwrap_or(step_id)),
          RunnerEvent::JobCompleted {
            conclusion,
            outputs,
            ..
          } => completed = Some((conclusion, outputs)),
          RunnerEvent::StepSummary { .. }
          | RunnerEvent::JobStarted { .. }
          | RunnerEvent::StepStarted { .. }
          | RunnerEvent::StepCompleted { .. }
          | RunnerEvent::StepSkipped { .. }
          | RunnerEvent::Log { .. }
          | RunnerEvent::LogGroup { .. }
          | RunnerEvent::Annotation { .. }
          | RunnerEvent::StepMetadata { .. }
          | RunnerEvent::InfrastructureError { .. } => {},
        }
      }
      (failed, completed)
    })
    .await;
    cancel.cancel();
    collected
  })?;
  let (conclusion, outputs) = completed.ok_or("job never completed")?;
  let marker = std::fs::read_to_string(workspace.join("docker-103.txt")).ok();
  Ok(Replayed {
    conclusion,
    failed,
    outputs,
    marker,
  })
}

#[test]
#[ignore = "requires Linux, real Docker, and a daemon-shared test root"]
fn captured_docker_job_executes_the_container_and_cleans_up() -> TestResult {
  let before = probe_containers()?;
  let replayed = replay(raw()?)?;
  assert_eq!(
    replayed.conclusion,
    Conclusion::Success,
    "failed: {:?}",
    replayed.failed
  );
  assert_eq!(
    replayed.outputs.get("release").map(String::as_str),
    Some("3.20.10")
  );
  assert_eq!(replayed.marker.as_deref(), Some("3.20.10\n"));
  assert_eq!(
    probe_containers()?,
    before,
    "the container action must be removed"
  );
  Ok(())
}

#[test]
#[ignore = "requires Linux, real Docker, and a daemon-shared test root"]
fn echoing_a_docker_string_instead_of_running_the_container_fails() -> TestResult {
  let mut message = raw()?;
  let steps = message
    .get_mut("steps")
    .and_then(serde_json::Value::as_array_mut)
    .ok_or("captured steps missing")?;
  let script = steps.first().ok_or("captured script step missing")?.clone();
  let container = steps.get_mut(1).ok_or("captured container step missing")?;
  let id = container.get("id").cloned().ok_or("container id missing")?;
  let context_name = container
    .get("contextName")
    .cloned()
    .ok_or("container contextName missing")?;
  // The pre-#103 "docker" live step: a shell echo that never runs a container.
  let mut echo = script;
  set(&mut echo, "/id", id)?;
  set(&mut echo, "/contextName", context_name)?;
  set(
    &mut echo,
    "/inputs/map/0/Value/lit",
    serde_json::json!("echo 'hello from a step that pulls docker://alpine:3.20'"),
  )?;
  *container = echo;
  let replayed = replay(message)?;
  assert_eq!(replayed.conclusion, Conclusion::Failure);
  assert_eq!(
    replayed.failed,
    vec!["__run_2".to_owned()],
    "the verify step must fail"
  );
  assert_eq!(replayed.marker, None, "no container wrote the marker");
  Ok(())
}

#[test]
#[ignore = "requires Linux, real Docker, and a daemon-shared test root"]
fn empty_who_input_reaches_the_container_and_fails_verification() -> TestResult {
  let mut message = raw()?;
  let inputs = message
    .pointer_mut("/contextData/inputs/d")
    .and_then(serde_json::Value::as_array_mut)
    .ok_or("captured inputs missing")?;
  let who = inputs
    .iter_mut()
    .find(|entry| entry.get("k").and_then(serde_json::Value::as_str) == Some("who"))
    .ok_or("captured who missing")?;
  set(who, "/v", serde_json::json!(""))?;
  let replayed = replay(message)?;
  assert_eq!(replayed.conclusion, Conclusion::Failure);
  assert_eq!(
    replayed.failed,
    vec!["__run_2".to_owned()],
    "the verify step must fail"
  );
  assert_eq!(
    replayed.marker.as_deref(),
    Some("3.20.10\n"),
    "the container still ran"
  );
  Ok(())
}
