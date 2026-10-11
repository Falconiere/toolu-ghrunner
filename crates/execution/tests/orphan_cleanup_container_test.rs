//! Issue #89 Linux lane: a captured job-container replay against real Docker.
//!
//! Container execs are never tagged with `RUNNER_TRACKING_ID` (upstream only
//! sets it on the worker's own environment, which `docker exec` does not
//! inherit); the host sweep still runs for the job and leaves the container's
//! processes to container teardown.

#![cfg(target_os = "linux")]

use std::error::Error;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use execution::Runner;
use shared::{
  AgentJobRequestMessage, Conclusion, RunnerConfig, RunnerEvent, SecretMasker, TemplateToken,
};
use tokio_util::sync::CancellationToken;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;
const CAPTURE: &str = include_str!("../../toolu-runner/tests/fixtures/job_container_message.json");
const SHELL: &str = "sleep 300 & printf 'TRACK89|container|%s\\n' \"${RUNNER_TRACKING_ID-unset}\"";

fn literal(value: &str) -> TemplateToken {
  TemplateToken {
    token_type: 0,
    lit: Some(value.to_owned()),
    ..TemplateToken::default()
  }
}

fn captured_shell_job() -> TestResult<AgentJobRequestMessage> {
  let mut job: AgentJobRequestMessage = serde_json::from_str(CAPTURE)?;
  job
    .steps
    .retain(|step| step.context_name.as_deref() == Some("shell"));
  let script = job
    .steps
    .first_mut()
    .and_then(|step| step.inputs.d.as_mut())
    .and_then(|entries| {
      entries
        .iter_mut()
        .find(|entry| entry.key.to_string_value() == Some("script"))
    })
    .ok_or("captured shell script input absent")?;
  script.value = literal(SHELL);
  Ok(job)
}

#[tokio::test]
#[ignore = "requires a Linux runner and real Docker daemon"]
async fn container_step_is_untagged_and_host_sweep_still_runs() -> TestResult {
  let base = std::env::var_os("TOOLU_CONTAINER_TEST_ROOT")
    .map(std::path::PathBuf::from)
    .ok_or("TOOLU_CONTAINER_TEST_ROOT must name a path shared with Docker")?;
  let root = tempfile::Builder::new()
    .prefix("orphan89 container ")
    .tempdir_in(base)?;
  let config = RunnerConfig {
    data_dir: root.path().join("runner data"),
    workspace_root: root.path().join("work root"),
    workspace_gc_hours: 0,
    ..RunnerConfig::default()
  };
  let runner = Runner::new(config, Arc::new(Mutex::new(SecretMasker::new())));
  let mut events = runner.execute_job(captured_shell_job()?, CancellationToken::new());
  let mut lines = Vec::new();
  let mut conclusion = None;
  tokio::time::timeout(Duration::from_secs(240), async {
    while let Some(event) = events.recv().await {
      if let RunnerEvent::Log { line, .. } = &event {
        lines.push(line.clone());
      }
      if let RunnerEvent::JobCompleted {
        conclusion: result, ..
      } = event
      {
        conclusion = Some(result);
      }
    }
  })
  .await?;
  assert_eq!(conclusion, Some(Conclusion::Success), "{lines:?}");
  let markers: Vec<&String> = lines
    .iter()
    .filter(|line| line.starts_with("TRACK89|"))
    .collect();
  assert_eq!(markers, ["TRACK89|container|unset"], "{lines:?}");
  assert!(
    lines
      .iter()
      .any(|line| line == "Cleaning up orphan processes"),
    "{lines:?}"
  );
  assert!(
    !lines
      .iter()
      .any(|line| line.starts_with("Terminate orphan process")),
    "{lines:?}"
  );
  Ok(())
}
