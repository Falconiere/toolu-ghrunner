//! AC-5: action pre/main/post stages + LIFO post-drain + `STATE_` cross-stage.
//!
//! Real-data, no mocks. Drives a committed node action fixture
//! (`fixtures/prepost_action/`, with real `pre.js`/`main.js`/`post.js`) through
//! the live step loop (`execution::steps_runner::run_steps`). The fixture's
//! `main` calls `::save-state name=k::…` and `post` reads `STATE_k`, writing
//! stage markers to a file so the test can assert:
//!   1. The POST stage runs at job end.
//!   2. With two action steps, posts drain in REVERSE (LIFO) order.
//!   3. POST still runs when a LATER step failed (`always()` semantics).
//!   4. POST sees `STATE_k` set by its own step's MAIN stage (cross-stage).
//!
//! The action is seeded into the runner's on-disk action cache and the system
//! `node` binary into the node cache, so the engine resolves both from disk —
//! no network, fully hermetic. If no real `node` is on `PATH`, the test skips.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use execution::execution::actions::downloader::{action_cache_dir, watermark_path};
use execution::execution::context::ExecutionContext;
use execution::execution::steps_runner::run_steps;
use execution::node::runtime::{node_binary_path, node_cache_dir, node_version_for};
use shared::SecretMasker;
use shared::{ActionStep, RunnerConfig, RunnerEvent};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

type TestResult<T> = Result<T, Box<dyn Error>>;

const FIXTURE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/prepost_action");

/// Locate a real `node` binary, resolving shims via `process.execPath`.
/// `None` => skip (no real node runtime available).
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

/// Seed the real node binary into the runner's node cache for `node20` so
/// `ensure_node_runtime` resolves it from disk instead of downloading. Uses a
/// symlink (preserving the install's sibling files), falling back to a copy.
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

/// Seed a copy of the fixture action into the action cache under
/// `{owner}/{repo}/{ref}` with the `marker` input default rewritten, then
/// drop the `.completed` watermark so `is_action_cached` returns true.
fn seed_action(data_dir: &Path, repo: &str, marker: &str) -> TestResult<()> {
  let cache_key = format!("toolu/{repo}/v1");
  let cache_dir = action_cache_dir(data_dir, &cache_key);
  std::fs::create_dir_all(&cache_dir)?;

  for name in ["action.yml", "pre.js", "main.js", "post.js"] {
    let src = Path::new(FIXTURE_DIR).join(name);
    let contents = std::fs::read_to_string(&src)?;
    let patched = if name == "action.yml" {
      contents.replace("default: 'X'", &format!("default: '{marker}'"))
    } else {
      contents
    };
    std::fs::write(cache_dir.join(name), patched)?;
  }

  std::fs::write(watermark_path(&cache_dir), "")?;
  Ok(())
}

/// Build an action step that resolves to the seeded `toolu/{repo}@v1` action.
fn action_step(id: &str, repo: &str) -> ActionStep {
  let mut step = ActionStep::with_ref_type(id, "repository");
  step.reference.name = Some(format!("toolu/{repo}"));
  step.reference.git_ref = Some("v1".to_owned());
  step
}

/// Drive `steps` through the live step loop with a pre-seeded action+node
/// cache and a `MARKER_FILE` env var; return the marker-file lines + events.
async fn run_with_markers(
  steps: Vec<ActionStep>,
  repos: &[(&str, &str)],
) -> TestResult<Option<(Vec<String>, Vec<RunnerEvent>)>> {
  let Some(node) = system_node() else {
    eprintln!("SKIP: no system `node` on PATH; pre/post test needs a real node runtime");
    return Ok(None);
  };

  let dir = tempfile::tempdir()?;
  let (workspace, data_dir) = (dir.path().join("work"), dir.path().join("data"));
  std::fs::create_dir_all(&workspace)?;
  std::fs::create_dir_all(&data_dir)?;
  let config = RunnerConfig {
    data_dir: data_dir.clone(),
    workspace_root: workspace.clone(),
    cgroup_path: None,
    services_mode: shared::ServicesMode::default(),
    ..RunnerConfig::default()
  };

  seed_node(&data_dir, &node)?;
  for (repo, marker) in repos {
    seed_action(&data_dir, repo, marker)?;
  }

  let marker_file = data_dir.join("markers.txt");
  std::fs::write(&marker_file, "")?;
  let events = drive(&steps, &workspace, &config, &marker_file).await?;

  let lines: Vec<String> = std::fs::read_to_string(&marker_file)?
    .lines()
    .map(ToOwned::to_owned)
    .collect();
  drop(dir);
  Ok(Some((lines, events)))
}

/// Run the step loop with `MARKER_FILE` set; collect every emitted event.
async fn drive(
  steps: &[ActionStep],
  workspace: &Path,
  config: &RunnerConfig,
  marker_file: &Path,
) -> TestResult<Vec<RunnerEvent>> {
  let (_conclusion, events) = drive_with_cancel(
    steps,
    workspace,
    config,
    marker_file,
    CancellationToken::new(),
  )
  .await?;
  Ok(events)
}

/// [`drive`] with an externally held cancel token, returning the job
/// conclusion too (for cancel-path tests).
async fn drive_with_cancel(
  steps: &[ActionStep],
  workspace: &Path,
  config: &RunnerConfig,
  marker_file: &Path,
  cancel: CancellationToken,
) -> TestResult<(shared::Conclusion, Vec<RunnerEvent>)> {
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
  let conclusion = run_steps(
    steps,
    &mut ctx,
    &tx,
    cancel,
    &execution::execution::steps_runner::JobRun {
      workspace,
      config,
      spec: &spec,
      shadow: None,
    },
  )
  .await?;
  drop(tx);
  Ok((conclusion, collector.await?))
}

/// 1 + 4: a single action's post runs at job end and sees its main's STATE_k.
#[tokio::test]
async fn post_runs_at_job_end_and_sees_main_state() -> TestResult<()> {
  let steps = vec![action_step("a", "act-a")];
  let Some((lines, _events)) = run_with_markers(steps, &[("act-a", "A")]).await? else {
    return Ok(());
  };

  assert_eq!(
    lines,
    vec![
      "A:pre".to_owned(),
      "A:main".to_owned(),
      "A:post:STATE_k=A-state".to_owned(),
    ],
    "expected pre -> main -> post with STATE_k visible to post; got {lines:?}"
  );
  Ok(())
}

/// 2: two action steps register posts; posts drain in REVERSE (LIFO) order.
#[tokio::test]
async fn two_posts_drain_in_reverse_lifo_order() -> TestResult<()> {
  let steps = vec![action_step("a", "act-a"), action_step("b", "act-b")];
  let repos = [("act-a", "A"), ("act-b", "B")];
  let Some((lines, _events)) = run_with_markers(steps, &repos).await? else {
    return Ok(());
  };

  // Mains run forward (A then B); posts drain LIFO (B then A).
  assert_eq!(
    lines,
    vec![
      "A:pre".to_owned(),
      "A:main".to_owned(),
      "B:pre".to_owned(),
      "B:main".to_owned(),
      "B:post:STATE_k=B-state".to_owned(),
      "A:post:STATE_k=A-state".to_owned(),
    ],
    "posts must drain LIFO (B before A) at job end; got {lines:?}"
  );
  Ok(())
}

/// 3: a LATER `run:` step fails, yet the action's post still runs (always()).
#[tokio::test]
async fn post_runs_even_when_a_later_step_failed() -> TestResult<()> {
  let mut steps = vec![action_step("a", "act-a")];
  // A later run step that fails. The step loop drains posts at job end either
  // way, so `continue_on_error = false` is what we want: it lets the failure
  // flip the job status, so `success()` posts would skip while `always()`
  // (the post-if default) must still fire.
  let mut failing = ActionStep::script("boom", "exit 1", "");
  failing.continue_on_error = Some(false);
  steps.push(failing);

  let Some((lines, _events)) = run_with_markers(steps, &[("act-a", "A")]).await? else {
    return Ok(());
  };

  let post = lines.iter().find(|l| l.starts_with("A:post:"));
  assert_eq!(
    post.map(String::as_str),
    Some("A:post:STATE_k=A-state"),
    "post (always()) must run after a later step failed; markers={lines:?}"
  );
  Ok(())
}

/// Sanity: the committed fixture parses and declares pre/main/post.
#[test]
fn fixture_declares_all_three_stages() -> TestResult<()> {
  let yml = std::fs::read_to_string(Path::new(FIXTURE_DIR).join("action.yml"))?;
  assert!(yml.contains("pre: 'pre.js'"), "fixture must declare pre");
  assert!(yml.contains("main: 'main.js'"), "fixture must declare main");
  assert!(yml.contains("post: 'post.js'"), "fixture must declare post");
  // No leftover placeholder; the test patches the marker default per-action.
  assert!(
    yml.contains("default: 'X'"),
    "marker default must be present"
  );
  Ok(())
}

/// `drive`, but returning the raw `run_steps` `Result` (for error-path tests
/// where the step loop is expected to surface a hard `Err`).
async fn drive_raw(
  steps: &[ActionStep],
  workspace: &Path,
  config: &RunnerConfig,
  marker_file: &Path,
) -> TestResult<Result<shared::Conclusion, shared::RunnerError>> {
  let masker = Arc::new(Mutex::new(SecretMasker::new()));
  let mut ctx = ExecutionContext::with_masker(Arc::clone(&masker));
  ctx.set_env("MARKER_FILE", &marker_file.to_string_lossy());

  let (tx, mut rx) = mpsc::channel::<RunnerEvent>(1024);
  let collector = tokio::spawn(async move { while rx.recv().await.is_some() {} });

  let spec = execution::execution::job_spec::JobSpec::default();
  let result = run_steps(
    steps,
    &mut ctx,
    &tx,
    CancellationToken::new(),
    &execution::execution::steps_runner::JobRun {
      workspace,
      config,
      spec: &spec,
      shadow: None,
    },
  )
  .await;
  drop(tx);
  collector.await?;
  Ok(result)
}

/// 3b: posts still drain when a later step returns a hard `Err` (action
/// resolution failure), not just a `Failure` conclusion — the drain must run
/// before the error propagates out of `run_steps`.
#[tokio::test]
async fn post_drains_even_when_a_later_step_errors_hard() -> TestResult<()> {
  let Some(node) = system_node() else {
    eprintln!("SKIP: no system `node` on PATH; pre/post test needs a real node runtime");
    return Ok(());
  };

  let dir = tempfile::tempdir()?;
  let (workspace, data_dir) = (dir.path().join("work"), dir.path().join("data"));
  std::fs::create_dir_all(&workspace)?;
  std::fs::create_dir_all(&data_dir)?;
  let config = RunnerConfig {
    data_dir: data_dir.clone(),
    workspace_root: workspace.clone(),
    cgroup_path: None,
    services_mode: shared::ServicesMode::default(),
    ..RunnerConfig::default()
  };
  seed_node(&data_dir, &node)?;
  seed_action(&data_dir, "act-a", "A")?;
  let marker_file = data_dir.join("markers.txt");
  std::fs::write(&marker_file, "")?;

  // Step 2 references a local action dir that does not exist, so the step
  // loop hits a hard resolution `Err` (not a `Failure` conclusion).
  let mut broken = ActionStep::with_ref_type("broken", "repository");
  broken.reference.name = Some("./does-not-exist".to_owned());
  broken.reference.git_ref = None;
  let steps = vec![action_step("a", "act-a"), broken];

  let result = drive_raw(&steps, &workspace, &config, &marker_file).await?;
  assert!(
    result.is_err(),
    "an unresolvable action must surface a hard Err from run_steps; got {result:?}"
  );

  let lines: Vec<String> = std::fs::read_to_string(&marker_file)?
    .lines()
    .map(ToOwned::to_owned)
    .collect();
  let post = lines.iter().find(|l| l.starts_with("A:post:"));
  assert_eq!(
    post.map(String::as_str),
    Some("A:post:STATE_k=A-state"),
    "post must drain even when a later step errors hard; markers={lines:?}"
  );
  Ok(())
}

/// A slow `main` for the cancel test: signals start, then stays alive far
/// longer than the test budget — only the job-cancel kill ends it before the
/// done marker is written.
const SLOW_MAIN_JS: &str = "\
const fs = require('fs');
const file = process.env.MARKER_FILE;
if (file) fs.appendFileSync(file, 'A:main-start\\n');
setTimeout(() => {
  if (file) fs.appendFileSync(file, 'A:main-done\\n');
}, 30000);
";

/// Fire `cancel` once the slow main's start marker is on disk, so the kill
/// hits a live child. The 10s ceiling turns a stuck spawn into assertion
/// failures instead of a suite hang.
fn cancel_when_main_starts(
  cancel: CancellationToken,
  marker: std::path::PathBuf,
) -> tokio::task::JoinHandle<()> {
  tokio::spawn(async move {
    let started = async {
      while !std::fs::read_to_string(&marker)
        .unwrap_or_default()
        .contains("A:main-start")
      {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
      }
    };
    let _ = tokio::time::timeout(std::time::Duration::from_secs(10), started).await;
    cancel.cancel();
  })
}

/// A cancelled job still drains its posts: the cancel kills the slow `main`
/// promptly, and the `post` stage then runs under the cancel-grace bounds
/// (a fresh token) instead of being killed by the already-fired job token.
#[tokio::test]
async fn post_drains_with_grace_when_job_is_cancelled() -> TestResult<()> {
  let Some(node) = system_node() else {
    eprintln!("SKIP: no system `node` on PATH; pre/post test needs a real node runtime");
    return Ok(());
  };

  let dir = tempfile::tempdir()?;
  let (workspace, data_dir) = (dir.path().join("work"), dir.path().join("data"));
  std::fs::create_dir_all(&workspace)?;
  std::fs::create_dir_all(&data_dir)?;
  let config = RunnerConfig {
    data_dir: data_dir.clone(),
    workspace_root: workspace.clone(),
    cgroup_path: None,
    services_mode: shared::ServicesMode::default(),
    ..RunnerConfig::default()
  };
  seed_node(&data_dir, &node)?;
  seed_action(&data_dir, "act-a", "A")?;
  // Swap the cached main for the slow variant AFTER seeding; pre/post stay real.
  let cache_dir = action_cache_dir(&data_dir, "toolu/act-a/v1");
  std::fs::write(cache_dir.join("main.js"), SLOW_MAIN_JS)?;
  let marker_file = data_dir.join("markers.txt");
  std::fs::write(&marker_file, "")?;

  let cancel = CancellationToken::new();
  let canceller = cancel_when_main_starts(cancel.clone(), marker_file.clone());

  let steps = vec![action_step("a", "act-a")];
  let started_at = std::time::Instant::now();
  let (conclusion, _events) =
    drive_with_cancel(&steps, &workspace, &config, &marker_file, cancel).await?;
  let elapsed = started_at.elapsed();
  canceller.await?;

  assert_eq!(
    conclusion,
    shared::Conclusion::Cancelled,
    "cancel during main must surface Cancelled"
  );
  assert!(
    elapsed < std::time::Duration::from_secs(15),
    "cancel must kill the 30s main promptly; took {elapsed:?}"
  );
  let lines: Vec<String> = std::fs::read_to_string(&marker_file)?
    .lines()
    .map(ToOwned::to_owned)
    .collect();
  assert!(
    lines.iter().any(|l| l == "A:main-start"),
    "slow main must have started before the cancel; markers={lines:?}"
  );
  assert!(
    !lines.iter().any(|l| l == "A:main-done"),
    "the cancel must kill main before its done marker; markers={lines:?}"
  );
  assert!(
    lines.iter().any(|l| l.starts_with("A:post:")),
    "the post must still run after a job cancel (cancel-grace bounds); markers={lines:?}"
  );
  Ok(())
}
