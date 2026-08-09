//! Composite `continue-on-error` regression coverage (production job
//! 93275790191): a composite's inner `continue-on-error: true` step must not
//! abort the composite, whether it fails via a normal non-zero exit or a hard
//! `Err` (action resolution/manifest read) escaping the inner step entirely.
//! Also covers the third diagnosed defect: a nested `uses:` step's own Log
//! events (action header) must land on the enclosing composite's real
//! top-level step id, not the synthetic id nobody uploads.
//!
//! Real-data, no mocks: drives the live step loop
//! (`execution::steps_runner::run_steps`) against committed local action
//! fixtures under `fixtures/local_actions/`, exactly like
//! `gh_compat_actions.rs`.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use execution::execution::context::ExecutionContext;
use execution::execution::steps_runner::run_steps;
use execution::node::runtime::{node_binary_path, node_cache_dir, node_version_for};
use shared::{ActionStep, Conclusion, RunnerConfig, RunnerEvent, SecretMasker};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

const FIXTURE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/local_actions");

/// Build a `uses: ./{path}` local action step.
fn local_action_step(id: &str, path: &str) -> ActionStep {
  let mut step = ActionStep::with_ref_type(id, "repository");
  step.reference.name = Some(format!("./{path}"));
  step.reference.git_ref = None;
  step
}

/// Drive a single local composite action step through the live engine, with
/// `MARKER_FILE` set so its `run:` steps record an ordered trace. Returns the
/// step's conclusion and every event the run produced.
async fn drive(
  data_dir: &Path,
  marker_file: &Path,
  action_path: &str,
) -> TestResult<(Conclusion, Vec<RunnerEvent>)> {
  let config = RunnerConfig {
    data_dir: data_dir.to_path_buf(),
    workspace_root: PathBuf::from(FIXTURE_DIR),
    cgroup_path: None,
    ..RunnerConfig::default()
  };
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

  let spec = execution::execution::job_spec::JobSpec::default();
  let http = reqwest::Client::new();
  let steps = vec![local_action_step("outer", action_path)];
  let conclusion = run_steps(
    &steps,
    &mut ctx,
    &tx,
    CancellationToken::new(),
    &execution::execution::steps_runner::JobRun {
      workspace: &workspace,
      config: &config,
      spec: &spec,
      shadow: None,
      http: &http,
    },
  )
  .await?;

  drop(tx);
  let events = collector.await?;
  Ok((conclusion, events))
}

/// Read the marker file's non-empty lines in order (missing file -> empty).
fn markers(path: &Path) -> Vec<String> {
  std::fs::read_to_string(path)
    .unwrap_or_default()
    .lines()
    .filter(|l| !l.trim().is_empty())
    .map(str::to_owned)
    .collect()
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
    None
  } else {
    Some(PathBuf::from(path))
  }
}

/// Seed the real node binary into the runner's node cache for `node20` so the
/// engine resolves it from disk instead of downloading (mirrors
/// `gh_compat_actions.rs`'s helper of the same name).
fn seed_node(data_dir: &Path, node: &Path) -> TestResult {
  let cache_dir = node_cache_dir(data_dir, node_version_for(20));
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

#[tokio::test]
async fn continue_on_error_survives_a_failing_inner_step_and_runs_the_next() -> TestResult {
  let tmp = tempfile::tempdir()?;
  let data_dir = tmp.path().join("data");
  std::fs::create_dir_all(&data_dir)?;
  let marker_file = tmp.path().join("markers.txt");

  let (conclusion, _events) = drive(&data_dir, &marker_file, "composite-continue-on-error").await?;

  // GitHub semantics: a composite whose failed inner step carries
  // `continue-on-error: true` still succeeds overall once every remaining
  // step has run — the failure never propagates past that inner step.
  assert_eq!(
    conclusion,
    Conclusion::Success,
    "continue-on-error inner failure must not fail the composite/parent step"
  );
  assert_eq!(
    markers(&marker_file),
    vec!["second-ran".to_owned()],
    "the step after the continue-on-error failure must still run"
  );
  Ok(())
}

#[tokio::test]
async fn without_continue_on_error_a_failing_inner_step_stops_the_composite() -> TestResult {
  let tmp = tempfile::tempdir()?;
  let data_dir = tmp.path().join("data");
  std::fs::create_dir_all(&data_dir)?;
  let marker_file = tmp.path().join("markers.txt");

  let (conclusion, _events) = drive(&data_dir, &marker_file, "composite-stop-on-error").await?;

  assert_eq!(
    conclusion,
    Conclusion::Failure,
    "a failing inner step with no continue-on-error must fail the composite"
  );
  assert_eq!(
    markers(&marker_file),
    Vec::<String>::new(),
    "the step after a non-continue-on-error failure must NOT run"
  );
  Ok(())
}

#[tokio::test]
async fn continue_on_error_survives_a_hard_error_from_a_nested_uses_step() -> TestResult {
  let tmp = tempfile::tempdir()?;
  let data_dir = tmp.path().join("data");
  std::fs::create_dir_all(&data_dir)?;
  let marker_file = tmp.path().join("markers.txt");

  let (conclusion, events) = drive(&data_dir, &marker_file, "composite-uses-hard-error").await?;

  // The hard `RunnerError::ActionManifest` (no action.yml under
  // `broken-no-manifest`) must surface as a `##[error]` Log scoped to the
  // OUTER step id ("outer") — composite inner steps have no GH step of
  // their own, so there is nowhere else for it to be visible.
  let error_line = events.iter().find_map(|e| {
    if let RunnerEvent::Log { step_id, line, .. } = e
      && step_id == "outer"
      && line.starts_with("##[error]")
    {
      return Some(line.clone());
    }
    None
  });
  assert!(
    error_line.is_some(),
    "expected a ##[error] Log scoped to the parent step id; events={events:?}"
  );

  assert_eq!(
    conclusion,
    Conclusion::Success,
    "continue-on-error must survive a hard error too, not just a script failure"
  );
  assert_eq!(
    markers(&marker_file),
    vec!["second-ran".to_owned()],
    "the step after the hard-error nested uses: step must still run"
  );
  Ok(())
}

/// Fix 3 regression: a composite's nested `uses:` step (`./node-leaf`, no
/// explicit `id:`, so its synthetic id is `__composite_uses_1`) built its
/// action-header `Log` events under that synthetic id, which the listener
/// never registers a per-step uploader for — the lines never reached
/// GitHub. They must carry the enclosing composite's real top-level step id
/// ("outer") instead. `emit_action_header` runs before the node runtime is
/// ever touched, so this holds even when `node` sub-cases elsewhere skip;
/// still seeds a real `node` when available to exercise the full path.
#[tokio::test]
async fn nested_uses_step_action_header_logs_land_on_the_parent_step_id() -> TestResult {
  let tmp = tempfile::tempdir()?;
  let data_dir = tmp.path().join("data");
  std::fs::create_dir_all(&data_dir)?;
  if let Some(node) = system_node() {
    seed_node(&data_dir, &node)?;
  }
  let marker_file = tmp.path().join("markers.txt");

  let (_conclusion, events) = drive(&data_dir, &marker_file, "composite-with-node").await?;

  let synthetic_id = "__composite_uses_1";
  assert!(
    !events
      .iter()
      .any(|e| matches!(e, RunnerEvent::Log { step_id, .. } if step_id == synthetic_id)),
    "no Log event should carry the synthetic nested-uses id; events={events:?}"
  );
  let header = events.iter().find_map(|e| {
    if let RunnerEvent::Log { step_id, line, .. } = e
      && step_id == "outer"
      && line.contains("uses: ./node-leaf")
    {
      return Some(line.clone());
    }
    None
  });
  assert!(
    header.is_some(),
    "expected the nested uses: step's action header on the parent step id; events={events:?}"
  );
  Ok(())
}
