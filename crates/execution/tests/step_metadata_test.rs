//! Issue 88: every reported row carries upstream's step telemetry
//! (`type` / `action_name` / `ref`), replaying the sanitized github.com
//! acquisition of `completejob-88.yml`'s `steps-toolu` job (run 38098611227)
//! with the committed `completejob-88-*` actions, real `bash`/`sh`, real Node
//! and (opt-in) real Docker.
//!
//! The captured `actions/checkout@v4` step is dropped: its sanitized
//! credentials cannot download from GitHub. Its remote-reference mapping is
//! covered by the `step_metadata` unit tests and the live run.

use std::collections::HashMap;
use std::error::Error;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use execution::Runner;
use execution::node::runtime::{node_binary_path, node_cache_dir, node_version_for};
use shared::{AgentJobRequestMessage, Conclusion, RunnerConfig, RunnerEvent, SecretMasker};
use tokio_util::sync::CancellationToken;

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

const CAPTURE: &str = include_str!("completejob_88_steps_message.json");
const ACTIONS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../.github/actions");

/// `(type, action_name, ref)` of one reported row.
type Metadata = (String, Option<String>, Option<String>);

fn captured(keep: &[usize]) -> TestResult<AgentJobRequestMessage> {
  let mut message: AgentJobRequestMessage = serde_json::from_str(CAPTURE)?;
  let steps = std::mem::take(&mut message.steps);
  message.steps = keep
    .iter()
    .map(|index| steps.get(*index).cloned().ok_or("captured step missing"))
    .collect::<Result<_, _>>()?;
  Ok(message)
}

fn config_for(root: &Path) -> RunnerConfig {
  RunnerConfig {
    data_dir: root.join("data"),
    workspace_root: root.join("work"),
    workspace_gc_hours: 0,
    ..RunnerConfig::default()
  }
}

/// Copy the committed probe actions where `uses: ./.github/actions/...` resolves.
fn seed_actions(workspace: &Path) -> TestResult {
  for name in [
    "completejob-88-node",
    "completejob-88-composite",
    "completejob-88-docker",
  ] {
    let source = Path::new(ACTIONS).join(name);
    let target = workspace.join(".github/actions").join(name);
    std::fs::create_dir_all(&target)?;
    for entry in std::fs::read_dir(&source)? {
      let entry = entry?;
      std::fs::copy(entry.path(), target.join(entry.file_name()))?;
    }
  }
  Ok(())
}

async fn seed_node(data: &Path) -> TestResult {
  let output = tokio::process::Command::new("node")
    .args(["-e", "process.stdout.write(process.execPath)"])
    .output()
    .await?;
  assert!(output.status.success(), "Node is required for this test");
  let host_binary = String::from_utf8(output.stdout)?;
  let target = node_binary_path(&node_cache_dir(data, node_version_for(20)));
  std::fs::create_dir_all(target.parent().ok_or("node cache parent missing")?)?;
  #[cfg(unix)]
  std::os::unix::fs::symlink(host_binary.trim(), target)?;
  #[cfg(not(unix))]
  std::fs::copy(host_binary.trim(), target)?;
  Ok(())
}

/// The reported rows of one replayed job.
struct Rows {
  /// `(step id, name, number)` in report order.
  started: Vec<(String, String, u32)>,
  /// Every `StepMetadata` event per step id, in emission order.
  metadata: HashMap<String, Vec<Metadata>>,
  conclusions: HashMap<String, Conclusion>,
  job: Option<Conclusion>,
}

async fn replay(message: AgentJobRequestMessage, root: &Path) -> TestResult<Rows> {
  let config = config_for(root);
  seed_actions(&config.workspace_root.join(&message.job_id))?;
  seed_node(&config.data_dir).await?;
  let runner = Runner::new(config, Arc::new(Mutex::new(SecretMasker::new())));
  let mut stream = runner.execute_job(message, CancellationToken::new());
  let mut rows = Rows {
    started: Vec::new(),
    metadata: HashMap::new(),
    conclusions: HashMap::new(),
    job: None,
  };
  tokio::time::timeout(Duration::from_secs(300), async {
    while let Some(event) = stream.recv().await {
      if let RunnerEvent::StepStarted {
        step_id,
        step_name,
        step_number,
      } = &event
      {
        rows
          .started
          .push((step_id.clone(), step_name.clone(), *step_number));
      }
      if let RunnerEvent::StepMetadata {
        step_id,
        kind,
        action,
        git_ref,
      } = &event
      {
        rows.metadata.entry(step_id.clone()).or_default().push((
          kind.clone(),
          action.clone(),
          git_ref.clone(),
        ));
      }
      if let RunnerEvent::StepCompleted {
        step_id,
        conclusion,
        ..
      } = &event
      {
        rows.conclusions.insert(step_id.clone(), *conclusion);
      }
      if let RunnerEvent::JobCompleted { conclusion, .. } = &event {
        rows.job = Some(*conclusion);
      }
    }
  })
  .await?;
  Ok(rows)
}

impl Rows {
  /// The single metadata triple of the row named `name`.
  fn of(&self, name: &str) -> TestResult<(&Metadata, Conclusion)> {
    let (id, _, _) = self
      .started
      .iter()
      .find(|(_, row, _)| row == name)
      .ok_or_else(|| format!("row {name:?} missing: {:?}", self.started))?;
    let metadata = self
      .metadata
      .get(id)
      .ok_or_else(|| format!("row {name:?} has no metadata"))?;
    assert_eq!(metadata.len(), 1, "{name}: one metadata event per row");
    let first = metadata.first().ok_or("metadata missing")?;
    let conclusion = *self.conclusions.get(id).ok_or("row not completed")?;
    Ok((first, conclusion))
  }

  /// Every reported row is labelled exactly once and nothing else is.
  fn assert_every_row_labelled(&self) {
    for (id, name, _) in &self.started {
      assert_eq!(
        self.metadata.get(id).map(Vec::len),
        Some(1),
        "{name} must carry exactly one metadata event"
      );
    }
    assert_eq!(self.metadata.len(), self.started.len());
  }
}

fn meta(kind: &str, action: &str) -> Metadata {
  (kind.to_owned(), Some(action.to_owned()), None)
}

#[tokio::test]
async fn script_node_composite_and_runner_rows_carry_upstream_telemetry() -> TestResult {
  let dir = tempfile::tempdir()?;
  // bash, sh, local node with post, local composite, failing continue-on-error.
  let rows = replay(captured(&[1, 2, 3, 4, 7])?, dir.path()).await?;
  assert_eq!(rows.job, Some(Conclusion::Success));
  assert_eq!(
    rows.of("Bash script")?,
    (&meta("run", "bash"), Conclusion::Success)
  );
  assert_eq!(
    rows.of("Sh script")?,
    (&meta("run", "sh"), Conclusion::Success)
  );
  let node = meta("node20", "./.github/actions/completejob-88-node");
  assert_eq!(
    rows.of("Local node with post")?,
    (&node, Conclusion::Success)
  );
  assert_eq!(
    rows.of("Post Local node with post")?,
    (&node, Conclusion::Success)
  );
  assert_eq!(
    rows.of("Local composite")?,
    (
      &meta("composite", "./.github/actions/completejob-88-composite"),
      Conclusion::Success
    )
  );
  // continue-on-error turns the failed outcome into a successful conclusion.
  assert_eq!(
    rows.of("Failing step with an error annotation")?,
    (&meta("run", "bash"), Conclusion::Success)
  );
  assert_eq!(
    rows.of("Complete job")?,
    (&meta("runner", "complete_job"), Conclusion::Success)
  );
  // Composite children are not rows and are never labelled.
  rows.assert_every_row_labelled();
  let complete = rows
    .started
    .iter()
    .find(|(_, name, _)| name == "Complete job")
    .map(|(_, _, number)| *number);
  let highest = rows.started.iter().map(|(_, _, number)| *number).max();
  assert_eq!(complete, highest, "Complete job is the last row");
  Ok(())
}

#[cfg(target_os = "linux")]
fn docker_root() -> TestResult<tempfile::TempDir> {
  let base = std::env::var_os("TOOLU_CONTAINER_TEST_ROOT")
    .map(std::path::PathBuf::from)
    .ok_or("TOOLU_CONTAINER_TEST_ROOT must be shared with the Docker daemon")?;
  Ok(
    tempfile::Builder::new()
      .prefix("step metadata ")
      .tempdir_in(base)?,
  )
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires Linux, real Docker, and a daemon-shared test root"]
async fn docker_rows_are_dockerfile_and_dockerhub() -> TestResult {
  let dir = docker_root()?;
  // Local Dockerfile action and the `docker://alpine:3.20` registry step.
  let rows = replay(captured(&[5, 6])?, dir.path()).await?;
  assert_eq!(rows.job, Some(Conclusion::Success));
  assert_eq!(
    rows.of("Local Dockerfile")?,
    (
      &meta("Dockerfile", "./.github/actions/completejob-88-docker"),
      Conclusion::Success
    )
  );
  assert_eq!(
    rows.of("Registry image")?,
    (&meta("DockerHub", "alpine:3.20"), Conclusion::Success)
  );
  rows.assert_every_row_labelled();
  Ok(())
}
