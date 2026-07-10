//! AC-6: local `./` actions + composite nested `uses:` recursion.
//!
//! Real-data, no mocks. Drives committed local action fixtures
//! (`fixtures/local_actions/`) through the live step loop
//! (`execution::steps_runner::run_steps`) with the workspace root set to the
//! fixtures dir so `uses: ./x` resolves to a checked-out repo directory.
//!
//! Asserts:
//!   1. A local `./` composite action runs every step, including a nested
//!      `uses: ./composite-child` step (not skipped). Each step appends an
//!      ordered marker to a file the test reads back.
//!   2. Recursion bottoms out through a 2-level nested composite
//!      (parent -> child -> grandchild), proving the depth tracker bounds but
//!      does not block legitimate nesting.
//!   3. A local node action invoked via `uses: ./node-leaf` runs its `main.js`
//!      (skipped gracefully if no real `node` is on PATH), including when
//!      invoked as a nested step from inside a composite.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use shared::{ActionStep, Conclusion, RunnerConfig, RunnerEvent};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use toolu_runner::execution::context::ExecutionContext;
use toolu_runner::execution::secret_masker::SecretMasker;
use toolu_runner::execution::steps_runner::run_steps;
use toolu_runner::node::runtime::{node_binary_path, node_cache_dir, node_version_for};

type TestResult<T> = Result<T, Box<dyn Error>>;

const FIXTURE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/local_actions");

/// Build a `uses: ./{path}` local action step.
fn local_action_step(id: &str, path: &str) -> ActionStep {
  let mut step = ActionStep::with_ref_type(id, "repository");
  step.reference.name = Some(format!("./{path}"));
  step.reference.git_ref = None;
  step
}

/// Locate a real `node` binary (resolving shims). `None` => skip node sub-case.
fn system_node() -> Option<PathBuf> {
  let out = std::process::Command::new("node")
    .args(["-e", "process.stdout.write(process.execPath)"])
    .output()
    .ok()?;
  if !out.status.success() {
    return None;
  }
  let path = String::from_utf8(out.stdout).ok()?.trim().to_owned();
  if path.is_empty() {
    return None;
  }
  Some(PathBuf::from(path))
}

/// Seed the real node binary into the runner's node cache for `node20` so the
/// engine resolves it from disk instead of downloading.
fn seed_node(data_dir: &Path, node: &Path) -> TestResult<()> {
  let version = node_version_for(20);
  let cache_dir = node_cache_dir(data_dir, version);
  let binary = node_binary_path(&cache_dir);
  if let Some(parent) = binary.parent() {
    std::fs::create_dir_all(parent)?;
  }
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt;
    if std::os::unix::fs::symlink(node, &binary).is_err() {
      std::fs::copy(node, &binary)?;
    }
    if let Ok(meta) = std::fs::metadata(&binary) {
      let mut perms = meta.permissions();
      perms.set_mode(0o755);
      let _ = std::fs::set_permissions(&binary, perms);
    }
  }
  #[cfg(not(unix))]
  {
    std::fs::copy(node, &binary)?;
  }
  Ok(())
}

/// Build a `RunnerConfig` whose `data_dir` is a temp dir and whose
/// `workspace_root`/`workspace` is the local-actions fixture dir (so `./x`
/// resolves there).
fn config_in(data_dir: &Path) -> RunnerConfig {
  RunnerConfig {
    data_dir: data_dir.to_path_buf(),
    workspace_root: PathBuf::from(FIXTURE_DIR),
    cgroup_path: None,
    services_mode: shared::ServicesMode::default(),
    ..RunnerConfig::default()
  }
}

/// Drive a list of steps through the live engine with `workspace` = fixture dir
/// and `MARKER_FILE` set so steps record an ordered execution trace.
async fn drive(
  steps: &[ActionStep],
  data_dir: &Path,
  marker_file: &Path,
) -> TestResult<Conclusion> {
  drive_with_cancel(steps, data_dir, marker_file, CancellationToken::new()).await
}

/// [`drive`] with an externally held cancel token, so a test can fire a
/// job-level cancel while a step is in flight.
async fn drive_with_cancel(
  steps: &[ActionStep],
  data_dir: &Path,
  marker_file: &Path,
  cancel: CancellationToken,
) -> TestResult<Conclusion> {
  let config = config_in(data_dir);
  let workspace = PathBuf::from(FIXTURE_DIR);

  let masker = Arc::new(Mutex::new(SecretMasker::new()));
  let mut ctx = ExecutionContext::with_masker(Arc::clone(&masker));
  ctx.set_env("MARKER_FILE", &marker_file.to_string_lossy());

  let (tx, mut rx) = mpsc::channel::<RunnerEvent>(1024);
  let collector = tokio::spawn(async move {
    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
      events.push(event);
    }
    events
  });

  let spec = toolu_runner::execution::job_spec::JobSpec::default();
  let conclusion = run_steps(
    steps,
    &mut ctx,
    &tx,
    cancel,
    &toolu_runner::execution::steps_runner::JobRun {
      workspace: &workspace,
      config: &config,
      spec: &spec,
      shadow: None,
    },
  )
  .await?;

  drop(tx);
  let _ = collector.await;
  Ok(conclusion)
}

/// Read the marker file's non-empty lines in order.
fn markers(path: &Path) -> Vec<String> {
  std::fs::read_to_string(path)
    .unwrap_or_default()
    .lines()
    .filter(|l| !l.trim().is_empty())
    .map(str::to_owned)
    .collect()
}

#[tokio::test]
async fn local_composite_runs_run_step_and_nested_uses_recursively() -> TestResult<()> {
  let tmp = tempfile::tempdir()?;
  let data_dir = tmp.path().join("data");
  std::fs::create_dir_all(&data_dir)?;
  let marker_file = tmp.path().join("markers.txt");

  // A single local composite step whose action.yml has a run: step AND a
  // nested `uses: ./composite-child` (which itself nests ./composite-grandchild).
  let steps = vec![local_action_step("parent", "composite-parent")];
  let conclusion = drive(&steps, &data_dir, &marker_file).await?;

  assert_eq!(conclusion, Conclusion::Success, "job should succeed");

  let lines = markers(&marker_file);
  assert_eq!(
    lines,
    vec![
      "parent-run".to_owned(),
      "child-run".to_owned(),
      "grandchild-run".to_owned(),
    ],
    "every nested step (including 2 levels of nested composite uses:) must run \
     in order; got: {lines:?}"
  );
  Ok(())
}

#[tokio::test]
async fn local_node_action_via_uses_runs_main() -> TestResult<()> {
  let Some(node) = system_node() else {
    eprintln!("skipping: no real `node` on PATH");
    return Ok(());
  };

  let tmp = tempfile::tempdir()?;
  let data_dir = tmp.path().join("data");
  std::fs::create_dir_all(&data_dir)?;
  seed_node(&data_dir, &node)?;
  let marker_file = tmp.path().join("markers.txt");

  // Top-level local node action invoked via `uses: ./node-leaf`.
  let steps = vec![local_action_step("leaf", "node-leaf")];
  let conclusion = drive(&steps, &data_dir, &marker_file).await?;

  assert_eq!(
    conclusion,
    Conclusion::Success,
    "node action should succeed"
  );
  let lines = markers(&marker_file);
  assert_eq!(
    lines,
    vec!["node-leaf-main".to_owned()],
    "local node action main.js must run; got: {lines:?}"
  );
  Ok(())
}

/// Fire `cancel` once the sleeper's start marker is on disk, so the kill
/// exercises a live child rather than racing the spawn. The 10s ceiling turns
/// a stuck spawn into an assertion failure instead of a suite hang.
fn cancel_when_sleeper_starts(
  cancel: CancellationToken,
  marker: PathBuf,
) -> tokio::task::JoinHandle<()> {
  tokio::spawn(async move {
    let started = async {
      while !std::fs::read_to_string(&marker)
        .unwrap_or_default()
        .contains("sleeper-start")
      {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
      }
    };
    let _ = tokio::time::timeout(std::time::Duration::from_secs(10), started).await;
    cancel.cancel();
  })
}

#[tokio::test]
async fn top_level_cancel_kills_nested_node_action_in_composite() -> TestResult<()> {
  let Some(node) = system_node() else {
    eprintln!("skipping: no real `node` on PATH");
    return Ok(());
  };

  let tmp = tempfile::tempdir()?;
  let data_dir = tmp.path().join("data");
  std::fs::create_dir_all(&data_dir)?;
  seed_node(&data_dir, &node)?;
  let marker_file = tmp.path().join("markers.txt");

  let cancel = CancellationToken::new();
  let canceller = cancel_when_sleeper_starts(cancel.clone(), marker_file.clone());

  // Composite: nested `uses: ./node-sleeper` (30s) then a run: step.
  let steps = vec![local_action_step("sleeper", "composite-with-sleeper")];
  let started_at = std::time::Instant::now();
  let conclusion = drive_with_cancel(&steps, &data_dir, &marker_file, cancel).await?;
  let elapsed = started_at.elapsed();
  canceller.await?;

  assert_eq!(
    conclusion,
    Conclusion::Cancelled,
    "a job cancel during a nested `uses:` step must surface Cancelled"
  );
  assert!(
    elapsed < std::time::Duration::from_secs(15),
    "cancel must kill the nested 30s sleeper promptly; took {elapsed:?}"
  );
  let lines = markers(&marker_file);
  assert_eq!(
    lines,
    vec!["sleeper-start".to_owned()],
    "the killed sleeper must not write its done marker, and the composite \
     must not run steps after the cancelled nested step; got: {lines:?}"
  );
  Ok(())
}

#[tokio::test]
async fn local_composite_with_nested_node_uses_runs_both() -> TestResult<()> {
  let Some(node) = system_node() else {
    eprintln!("skipping: no real `node` on PATH");
    return Ok(());
  };

  let tmp = tempfile::tempdir()?;
  let data_dir = tmp.path().join("data");
  std::fs::create_dir_all(&data_dir)?;
  seed_node(&data_dir, &node)?;
  let marker_file = tmp.path().join("markers.txt");

  // Composite with a run: step AND a nested `uses: ./node-leaf` (with inputs).
  let steps = vec![local_action_step("withnode", "composite-with-node")];
  let conclusion = drive(&steps, &data_dir, &marker_file).await?;

  assert_eq!(conclusion, Conclusion::Success, "composite should succeed");
  let lines = markers(&marker_file);
  assert_eq!(
    lines,
    vec!["with-node-run".to_owned(), "with-node-leaf".to_owned()],
    "composite run: step and nested local node action must both run, and \
     `with: marker` must reach the node action as INPUT_MARKER; got: {lines:?}"
  );
  Ok(())
}
