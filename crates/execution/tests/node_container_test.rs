//! Opt-in Linux Docker lane for #95: Node actions inside real job containers
//! run a runtime that starts there, and the host libc rule holds on real
//! image filesystems.
//!
//! The production `Runner::execute_job` path replays the captured github.com
//! job-container acquisition `toolu-runner/tests/fixtures/job_container_message.json`
//! (live flags: `requirenode24 = "true"`; its captured `jobContainer` token
//! with only the image literal swapped for a pinned `alpine` or `ubuntu`
//! digest) with its steps replaced by the committed `node20` probe. Runtimes are downloaded for real
//! (`actions/alpine_nodejs` / nodejs.org) into a fresh data dir.
#![cfg(target_os = "linux")]

use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use execution::Runner;
use execution::node::platform::{Libc, detect_libc};
use shared::{
  ActionStep, AgentJobRequestMessage, Conclusion, RunnerConfig, RunnerEvent, SecretMasker,
};
use tokio_util::sync::CancellationToken;

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

const CAPTURE: &str = include_str!("../../toolu-runner/tests/fixtures/job_container_message.json");
const ALPINE: &str =
  "alpine@sha256:5291449c3df73caf6ed85e649dec1b9e818b39a5d8c871e97afc13e9cd5e8fa8";
const UBUNTU: &str =
  "ubuntu@sha256:534baea6a22c03a63003dbc8dbe78fe34bc0d7e595d9a9dc9834884ff530eb55";
const DEBIAN: &str =
  "debian@sha256:7c7b2c966bc9ee8cedfeef67e0e279108992c77681fa595db4a9d65c06ccc587";
/// The same probe the official-runner lane runs (`.github/workflows/node-runtime-95.yml`).
const ACTION_YML: &str = include_str!("../../../.github/actions/node-runtime-95/action.yml");
const ACTION_JS: &str = include_str!("../../../.github/actions/node-runtime-95/index.js");

/// What the action observed about its own runtime.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Observed {
  version: String,
  exec_path: String,
  glibc: Option<String>,
}

fn test_root() -> TestResult<tempfile::TempDir> {
  let base =
    std::env::var_os("TOOLU_CONTAINER_TEST_ROOT").map_or_else(std::env::temp_dir, PathBuf::from);
  Ok(
    tempfile::Builder::new()
      .prefix("node runtime ")
      .tempdir_in(base)?,
  )
}

async fn run_in_container(image: &str) -> TestResult<(Option<Conclusion>, Observed, Vec<String>)> {
  let root = test_root()?;
  let config = RunnerConfig {
    data_dir: root.path().join("runner data"),
    workspace_root: root.path().join("work root"),
    workspace: shared::WorkspaceConfig {
      mode: shared::WorkspaceMode::Fresh,
      stable_gc_hours: 0,
    },
    ..RunnerConfig::default()
  };
  let mut captured: serde_json::Value = serde_json::from_str(CAPTURE)?;
  let image_literal = captured
    .pointer_mut("/jobContainer/map/0/Value/lit")
    .ok_or("captured jobContainer image literal missing")?;
  *image_literal = serde_json::Value::from(image);
  let mut job: AgentJobRequestMessage = serde_json::from_value(captured)?;
  let mut step = ActionStep::with_ref_type("node_probe", "repository");
  step.reference.path = Some("./node action".to_owned());
  step.reference.repository_type = Some("self".to_owned());
  job.steps = vec![step];
  let workspace = config.workspace_root.join(&job.job_id);
  let action = workspace.join("node action");
  tokio::fs::create_dir_all(&action).await?;
  tokio::fs::write(action.join("action.yml"), ACTION_YML).await?;
  tokio::fs::write(action.join("index.js"), ACTION_JS).await?;
  let runner = Runner::new(config, Arc::new(Mutex::new(SecretMasker::new())));
  let mut events = runner.execute_job(job, CancellationToken::new());
  let (conclusion, logs) = tokio::time::timeout(Duration::from_secs(600), async {
    let mut conclusion = None;
    let mut logs = Vec::new();
    while let Some(event) = events.recv().await {
      if let RunnerEvent::JobCompleted { conclusion: c, .. } = event {
        conclusion = Some(c);
      } else if let RunnerEvent::Log { line, .. } = event {
        logs.push(line);
      }
    }
    (conclusion, logs)
  })
  .await?;
  let report = workspace.join("node-runtime-95.json");
  let observed = tokio::fs::read_to_string(&report).await.map_err(|e| {
    format!(
      "action wrote no {} ({e}); logs: {logs:#?}",
      report.display()
    )
  })?;
  Ok((conclusion, serde_json::from_str(&observed)?, logs))
}

#[tokio::test]
#[ignore = "requires Linux x64, real Docker, and Node.js download access"]
async fn alpine_job_container_runs_node20_action_on_musl_node24() -> TestResult {
  let (conclusion, observed, logs) = run_in_container(ALPINE).await?;
  assert_eq!(conclusion, Some(Conclusion::Success), "{logs:#?}");
  assert!(observed.version.starts_with("v24."), "{}", observed.version);
  assert_eq!(observed.glibc, None, "musl runtime has no glibc");
  assert_eq!(observed.exec_path, "/github/node/24.21.0-alpine/bin/node");
  Ok(())
}

#[tokio::test]
#[ignore = "requires Linux x64, real Docker, and Node.js download access"]
async fn ubuntu_job_container_runs_node20_action_on_glibc_node24() -> TestResult {
  let (conclusion, observed, logs) = run_in_container(UBUNTU).await?;
  assert_eq!(conclusion, Some(Conclusion::Success), "{logs:#?}");
  assert!(observed.version.starts_with("v24."), "{}", observed.version);
  assert!(observed.glibc.is_some(), "glibc runtime reports its glibc");
  assert_eq!(observed.exec_path, "/github/node/24.21.0/bin/node");
  Ok(())
}

fn docker(args: &[&str]) -> TestResult<String> {
  let output = Command::new("docker").args(args).output()?;
  if !output.status.success() {
    return Err(
      format!(
        "docker {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
      )
      .into(),
    );
  }
  Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

/// `docker export` a container's filesystem into `dest` (device nodes skipped).
fn export_root(container: &str, dest: &Path) -> TestResult {
  let tarball = dest.with_extension("tar");
  docker(&["export", "-o", &tarball.to_string_lossy(), container])?;
  std::fs::create_dir_all(dest)?;
  let mut archive = tar::Archive::new(std::fs::File::open(&tarball)?);
  for entry in archive.entries()? {
    let mut entry = entry?;
    let kind = entry.header().entry_type();
    if kind.is_block_special() || kind.is_character_special() || kind.is_fifo() {
      continue;
    }
    entry.unpack_in(dest)?;
  }
  Ok(())
}

fn exported(image: &str, setup: Option<&str>, dest: &Path) -> TestResult {
  let id = match setup {
    Some(script) => docker(&["run", "-d", image, "sh", "-c", script])?,
    None => docker(&["create", image])?,
  };
  if setup.is_some() {
    docker(&["wait", &id])?;
  }
  let result = export_root(&id, dest);
  docker(&["rm", "-f", &id])?;
  result
}

#[test]
#[ignore = "requires real Docker and Debian package download access"]
fn exported_image_roots_classify_like_their_loaders() -> TestResult {
  let root = test_root()?;
  let alpine = root.path().join("alpine");
  exported(ALPINE, None, &alpine)?;
  assert_eq!(detect_libc(&alpine), Libc::Musl);
  let debian = root.path().join("debian");
  exported(DEBIAN, None, &debian)?;
  assert_eq!(detect_libc(&debian), Libc::Glibc);
  // Debian's `musl` package installs `/lib/ld-musl-x86_64.so.1` beside glibc.
  let with_musl = root.path().join("debian-musl");
  exported(
    DEBIAN,
    Some("apt-get update -qq && apt-get install -y -qq musl >/dev/null"),
    &with_musl,
  )?;
  let musl_loader = std::fs::read_dir(with_musl.join("lib"))?
    .filter_map(Result::ok)
    .any(|entry| entry.file_name().to_string_lossy().starts_with("ld-musl-"));
  assert!(
    musl_loader,
    "the musl package must have installed its loader"
  );
  assert_eq!(detect_libc(&with_musl), Libc::Glibc);
  Ok(())
}
