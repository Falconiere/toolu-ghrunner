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
//! The action is installed in the workspace and the system `node` binary
//! seeded into the node cache, so the engine resolves both from disk —
//! no network, fully hermetic. If no real `node` is on `PATH`, the test skips.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

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

/// Seed the real node binary into the runner's node cache for both node pins (node20 and node24) so
/// `ensure_node_runtime` resolves it from disk instead of downloading. Uses a
/// symlink (preserving the install's sibling files), falling back to a copy.
fn seed_node(data_dir: &Path, node: &Path) -> TestResult<()> {
  for major in [20, 24] {
    let version = node_version_for(major);
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
  }
  Ok(())
}

/// Install the committed fixture under `./{repo}` with its marker default
/// rewritten for this action instance.
fn install_action(workspace: &Path, repo: &str, marker: &str) -> TestResult<()> {
  let action_dir = workspace.join(repo);
  std::fs::create_dir_all(&action_dir)?;

  for name in ["action.yml", "pre.js", "main.js", "post.js"] {
    let src = Path::new(FIXTURE_DIR).join(name);
    let contents = std::fs::read_to_string(&src)?;
    let patched = if name == "action.yml" {
      contents.replace("default: 'X'", &format!("default: '{marker}'"))
    } else {
      contents
    };
    std::fs::write(action_dir.join(name), patched)?;
  }
  Ok(())
}

/// Build an action step that resolves to the installed local `./{repo}` action.
fn action_step(id: &str, repo: &str) -> ActionStep {
  let mut step = ActionStep::with_ref_type(id, "repository");
  step.reference.name = Some(format!("./{repo}"));
  step
}

/// Drive `steps` through the live step loop with local actions, a seeded Node
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
    install_action(&workspace, repo, marker)?;
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
  let http = reqwest::Client::new();
  let fetcher =
    execution::execution::actions::prefetch::ActionFetcher::new(&shared::NetworkConfig::default())?;
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
      http: &http,
      fetcher: &fetcher,
    },
  )
  .await?;
  drop(tx);
  Ok((conclusion, collector.await?))
}

/// 1 + 4: a single action's post runs at job end and sees its main's `STATE_k`.
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

/// #251 D3: an action from the workflow's own repository (`repositoryType:
/// self`, as GitHub delivers `uses: ./path`) runs no `pre`; its main step
/// warns with upstream's message and annotation (v2.338.2 `ActionRunner`).
/// Paired run 42403405937 recorded no `Pre` row on the official runner.
#[tokio::test]
async fn local_action_runs_no_pre_and_warns() -> TestResult<()> {
  let mut step = action_step("a", "act-a");
  step.reference.name = None;
  step.reference.repository_type = Some("self".to_owned());
  step.reference.path = Some("./act-a".to_owned());
  let Some((lines, events)) = run_with_markers(vec![step], &[("act-a", "A")]).await? else {
    return Ok(());
  };
  assert_eq!(
    lines,
    vec!["A:main".to_owned(), "A:post:STATE_k=A-state".to_owned()]
  );
  let message = "`pre` execution is not supported for local action from './act-a'";
  assert!(events.iter().any(|event| matches!(event,
    RunnerEvent::Annotation { level: shared::AnnotationLevel::Warning, message: m, .. } if m == message)));
  assert!(!events.iter().any(|event| matches!(event,
    RunnerEvent::StepStarted { step_name, .. } if step_name.starts_with("Pre "))));
  Ok(())
}

/// #251 review cr-3272e06767da1856: the pre-job phase evaluates
/// `timeout-minutes` only for a step that has a `pre`; an action without one
/// gets its timeout diagnostic once, at its main step, never before it.
#[tokio::test]
async fn a_step_without_pre_reports_its_timeout_error_once_at_main() -> TestResult<()> {
  let manifest = std::fs::read_to_string(Path::new(FIXTURE_DIR).join("action.yml"))?
    .replace("  pre: 'pre.js'\n", "");
  let mut step = action_step("a", "act-a");
  step.timeout_in_minutes = Some(shared::TemplateToken::expression("fromJSON('not json')"));
  let Some((_, events, markers)) = run_patched(step, &[("action.yml", &manifest)]).await? else {
    return Ok(());
  };
  assert!(!markers.iter().any(|m| m == "A:pre"), "{markers:?}");
  let mut started = false;
  let mut errors = Vec::new();
  for event in &events {
    if let RunnerEvent::StepStarted { step_id, .. } = event
      && step_id == "a"
    {
      started = true;
    } else if let RunnerEvent::Log { step_id, line, .. } = event
      && step_id == "a"
      && line.contains("determine the step timeout")
    {
      errors.push((started, line.clone()));
    }
  }
  assert_eq!(errors.len(), 1, "{errors:?}");
  assert!(
    errors.iter().all(|(after_start, _)| *after_start),
    "{errors:?}"
  );
  Ok(())
}

/// One `act-a` step (its `pre.js` set to `pre_js`) through the pre-job phase,
/// with an unexecutable node runtime when `broken_node`, cancelling after `cancel_after`.
/// A missing system `node` fails the test: it is a prerequisite, not a skip.
async fn run_pre_phase_case(
  pre_js: &str,
  broken_node: bool,
  cancel_after: Option<std::time::Duration>,
) -> TestResult<(shared::Conclusion, Vec<RunnerEvent>)> {
  let node = system_node().ok_or("prerequisite missing: no system `node` on PATH")?;
  let dir = tempfile::tempdir()?;
  let (workspace, data_dir) = (dir.path().join("work"), dir.path().join("data"));
  std::fs::create_dir_all(&workspace)?;
  std::fs::create_dir_all(&data_dir)?;
  let config = RunnerConfig {
    data_dir: data_dir.clone(),
    workspace_root: workspace.clone(),
    ..RunnerConfig::default()
  };
  seed_node(&data_dir, &node)?;
  if broken_node {
    // A runtime that cannot be executed: the stage fails hard, not by exit code.
    for major in [20, 24] {
      let binary = node_binary_path(&node_cache_dir(&data_dir, node_version_for(major)));
      std::fs::remove_file(&binary)?;
      std::fs::write(&binary, "not an executable")?;
    }
  }
  install_action(&workspace, "act-a", "A")?;
  std::fs::write(workspace.join("act-a").join("pre.js"), pre_js)?;
  let marker_file = data_dir.join("markers.txt");
  std::fs::write(&marker_file, "")?;
  let cancel = CancellationToken::new();
  if let Some(after) = cancel_after {
    let token = cancel.clone();
    tokio::spawn(async move {
      tokio::time::sleep(after).await;
      token.cancel();
    });
  }
  let (conclusion, events) = drive_with_cancel(
    &[action_step("a", "act-a")],
    &workspace,
    &config,
    &marker_file,
    cancel,
  )
  .await?;
  Ok((conclusion, events))
}

/// The id of the reported `Pre` row.
fn pre_row(events: &[RunnerEvent]) -> Option<String> {
  events.iter().find_map(|event| {
    if let RunnerEvent::StepStarted {
      step_id, step_name, ..
    } = event
      && step_name.starts_with("Pre ")
    {
      Some(step_id.clone())
    } else {
      None
    }
  })
}

/// #251 review cr-0c03427611c3940e: a hard `pre` error (an unusable runtime)
/// is logged on the Pre row before that row closes as failed, and no second
/// failed row appears.
#[tokio::test]
async fn a_hard_pre_error_is_logged_on_its_own_row() -> TestResult<()> {
  let (conclusion, events) = run_pre_phase_case("", true, None).await?;
  assert_eq!(conclusion, shared::Conclusion::Failure);
  let pre = pre_row(&events).ok_or("no Pre row")?;
  let completed = events
    .iter()
    .position(
      |event| matches!(event, RunnerEvent::StepCompleted { step_id, .. } if step_id == &pre),
    )
    .ok_or("Pre row never closed")?;
  assert!(
    events.iter().take(completed).any(|event| matches!(event,
      RunnerEvent::Log { step_id, line, .. } if step_id == &pre && line.starts_with("##[error]"))),
    "{events:?}"
  );
  assert_eq!(
    reported(&events, "Pre Run ./act-a"),
    Some(shared::Conclusion::Failure)
  );
  let pre_rows = events
    .iter()
    .filter(|event| matches!(event, RunnerEvent::StepStarted { step_name, .. } if step_name.starts_with("Pre ")))
    .count();
  assert_eq!(pre_rows, 1, "{events:?}");
  Ok(())
}

/// #251 review cr-0c03427611c3940e: a `pre` cancelled while it runs, or before
/// it starts, closes its row as cancelled (not failed), and the job ends
/// cancelled. Real cancellation coverage; it also passed before the fix.
#[tokio::test]
async fn a_cancelled_pre_stays_cancelled() -> TestResult<()> {
  let (conclusion, events) = run_pre_phase_case(
    "setTimeout(() => {}, 60000);\n",
    false,
    Some(std::time::Duration::from_secs(2)),
  )
  .await?;
  assert_eq!(
    reported(&events, "Pre Run ./act-a"),
    Some(shared::Conclusion::Cancelled),
    "{events:?}"
  );
  assert_eq!(conclusion, shared::Conclusion::Cancelled);
  // Cancelled before the stage starts: the row must not read as failed. (This
  // pins observed behavior; it does not reach the stage's cancellation-error
  // path, whose Cancelled mapping is verified by inspection only.)
  let (conclusion, events) = run_pre_phase_case("", false, Some(std::time::Duration::ZERO)).await?;
  assert_ne!(
    reported(&events, "Pre Run ./act-a"),
    Some(shared::Conclusion::Failure),
    "{events:?}"
  );
  assert_eq!(conclusion, shared::Conclusion::Cancelled);
  Ok(())
}

/// #251 review cr-3272e06767da1856: with a `pre`, an unevaluable
/// `timeout-minutes` is reported on the Pre row once it has started and on
/// the main row once that has started, never on a row before its start.
#[tokio::test]
async fn a_pre_reports_its_timeout_error_on_its_own_row() -> TestResult<()> {
  let mut step = action_step("a", "act-a");
  step.timeout_in_minutes = Some(shared::TemplateToken::expression("fromJSON('not json')"));
  let Some((_, events, _)) = run_patched(step, &[]).await? else {
    return Ok(());
  };
  let mut started = std::collections::HashSet::new();
  let mut rows = Vec::new();
  for event in &events {
    if let RunnerEvent::StepStarted {
      step_id, step_name, ..
    } = event
    {
      started.insert(step_id.clone());
      rows.push((step_id.clone(), step_name.clone()));
    } else if let RunnerEvent::Log { step_id, line, .. } = event
      && line.contains("determine the step timeout")
    {
      assert!(
        started.contains(step_id),
        "diagnostic before its row started: {step_id}"
      );
    }
  }
  let lines_for = |name_prefix: &str| -> usize {
    let ids: Vec<&str> = rows
      .iter()
      .filter(|(_, n)| n.starts_with(name_prefix))
      .map(|(i, _)| i.as_str())
      .collect();
    events
      .iter()
      .filter(|event| {
        matches!(event, RunnerEvent::Log { step_id, line, .. }
      if ids.contains(&step_id.as_str()) && line.contains("determine the step timeout"))
      })
      .count()
  };
  assert_eq!(lines_for("Pre "), 1, "{rows:?}");
  assert_eq!(lines_for("Run ./act-a"), 1, "{rows:?}");
  Ok(())
}

/// #251 review cr-f19c86978f0c20db: a `pre-if` that cannot be evaluated is a
/// visible failed `Pre` row with its error in the log, at the next number (no
/// hole before the workflow steps), and no stage runs.
#[tokio::test]
async fn an_unevaluable_pre_if_is_a_failed_pre_row() -> TestResult<()> {
  let manifest = std::fs::read_to_string(Path::new(FIXTURE_DIR).join("action.yml"))?.replace(
    "  pre: 'pre.js'",
    "  pre: 'pre.js'\n  pre-if: 'success( &&'",
  );
  let Some((conclusion, events, markers)) =
    run_patched(action_step("a", "act-a"), &[("action.yml", &manifest)]).await?
  else {
    return Ok(());
  };
  assert_eq!(conclusion, shared::Conclusion::Failure);
  assert!(
    !markers.iter().any(|m| m == "A:pre" || m == "A:main"),
    "{markers:?}"
  );
  let pre_id = events.iter().find_map(|event| {
    if let RunnerEvent::StepStarted {
      step_id,
      step_name,
      step_number,
    } = event
      && step_name.starts_with("Pre ")
    {
      assert_eq!(*step_number, 2, "the Pre row follows Set up job");
      Some(step_id.clone())
    } else {
      None
    }
  });
  let pre_id = pre_id.ok_or("no Pre row was reported")?;
  assert_eq!(
    reported(&events, "Pre Run ./act-a"),
    Some(shared::Conclusion::Failure)
  );
  assert!(
    events.iter().any(|event| matches!(event,
    RunnerEvent::Log { step_id, line, .. } if step_id == &pre_id && line.starts_with("##[error]")))
  );
  let main_number = events.iter().find_map(|event| {
    if let RunnerEvent::StepStarted {
      step_name,
      step_number,
      ..
    } = event
      && step_name == "Run ./act-a"
    {
      Some(*step_number)
    } else {
      None
    }
  });
  assert_eq!(main_number, Some(3), "the main row follows the Pre row");
  Ok(())
}

/// #251 D7: upstream runs every repository action's `pre` after "Set up job"
/// and before the first main step, as numbered rows in step order, and
/// registers the post then (paired run 42407955591: official `Pre` rows #2-#3
/// ran before `actions/checkout` #4). A `run:` step before both runs after them.
#[tokio::test]
async fn pre_stages_run_before_every_main_step_in_numbered_rows() -> TestResult<()> {
  let run = ActionStep::script("between", "echo run >> \"$MARKER_FILE\"", "success()");
  let steps = vec![run, action_step("a", "act-a"), action_step("b", "act-b")];
  let repos = [("act-a", "A"), ("act-b", "B")];
  let Some((lines, events)) = run_with_markers(steps, &repos).await? else {
    return Ok(());
  };
  let want = [
    "A:pre",
    "B:pre",
    "run",
    "A:main",
    "B:main",
    "B:post:STATE_k=B-state",
    "A:post:STATE_k=A-state",
  ];
  assert_eq!(lines, want.map(ToOwned::to_owned));
  let rows: Vec<(u32, String)> = events
    .iter()
    .filter_map(|event| {
      if let RunnerEvent::StepStarted {
        step_name,
        step_number,
        ..
      } = event
      {
        Some((*step_number, step_name.clone()))
      } else {
        None
      }
    })
    .collect();
  let pre_rows: Vec<u32> = rows
    .iter()
    .filter(|(_, n)| n.starts_with("Pre "))
    .map(|(i, _)| *i)
    .collect();
  assert_eq!(pre_rows, [2, 3], "pre rows follow Set up job: {rows:?}");
  let main_rows: Vec<u32> = rows
    .iter()
    .filter(|(_, n)| !n.starts_with("Pre ") && !n.starts_with("Post "))
    .map(|(i, _)| *i)
    .collect();
  assert_eq!(main_rows, [4, 5, 6], "{rows:?}");
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

  // Every pre runs first (upstream's pre-job phase, #251), mains run forward
  // (A then B), posts drain LIFO (B then A).
  assert_eq!(
    lines,
    vec![
      "A:pre".to_owned(),
      "B:pre".to_owned(),
      "A:main".to_owned(),
      "B:main".to_owned(),
      "B:post:STATE_k=B-state".to_owned(),
      "A:post:STATE_k=A-state".to_owned(),
    ],
    "posts must drain LIFO (B before A) at job end; got {lines:?}"
  );
  Ok(())
}

/// 3: a LATER `run:` step fails, yet the action's post still runs (`always()`).
#[tokio::test]
async fn post_runs_even_when_a_later_step_failed() -> TestResult<()> {
  let mut steps = vec![action_step("a", "act-a")];
  // A later run step that fails. The step loop drains posts at job end either
  // way, so `continue_on_error = false` is what we want: it lets the failure
  // flip the job status, so `success()` posts would skip while `always()`
  // (the post-if default) must still fire.
  let mut failing = ActionStep::script("boom", "exit 1", "");
  failing.set_continue_on_error(false);
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

/// Run one step against the fixture action installed as `./act-a` with the
/// named stage scripts replaced, returning the job conclusion, every event
/// and the marker lines. `None` when no real node runtime is available.
async fn run_patched(
  step: ActionStep,
  patches: &[(&str, &str)],
) -> TestResult<Option<(shared::Conclusion, Vec<RunnerEvent>, Vec<String>)>> {
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
    ..RunnerConfig::default()
  };
  seed_node(&data_dir, &node)?;
  install_action(&workspace, "act-a", "A")?;
  for (name, script) in patches {
    std::fs::write(workspace.join("act-a").join(name), script)?;
  }
  let marker_file = data_dir.join("markers.txt");
  std::fs::write(&marker_file, "")?;
  let (conclusion, events) = drive_with_cancel(
    &[step],
    &workspace,
    &config,
    &marker_file,
    CancellationToken::new(),
  )
  .await?;
  let markers = std::fs::read_to_string(&marker_file)?
    .lines()
    .map(ToOwned::to_owned)
    .collect();
  Ok(Some((conclusion, events, markers)))
}

/// The conclusion reported for the step started under `name`.
fn reported(events: &[RunnerEvent], name: &str) -> Option<shared::Conclusion> {
  let id = events.iter().find_map(|event| {
    if let RunnerEvent::StepStarted {
      step_id, step_name, ..
    } = event
      && step_name == name
    {
      Some(step_id)
    } else {
      None
    }
  })?;
  events.iter().find_map(|event| {
    if let RunnerEvent::StepCompleted {
      step_id,
      conclusion,
      ..
    } = event
      && step_id == id
    {
      Some(*conclusion)
    } else {
      None
    }
  })
}

/// 3b: like upstream, a post stage applies its step's `continue-on-error`
/// independently: a failing post with `continue-on-error: true` concludes
/// `success` and leaves the job green; without it the post fails the job.
/// It is named after the step's main-stage display name.
#[tokio::test]
async fn failing_post_honors_continue_on_error() -> TestResult<()> {
  for (continue_on_error, expected) in [
    (true, shared::Conclusion::Success),
    (false, shared::Conclusion::Failure),
  ] {
    let mut step = action_step("a", "act-a");
    step.set_continue_on_error(continue_on_error);
    step.display_name_token = Some(shared::TemplateToken::expression(
      "format('Deferred {0}', steps.prior.outputs.label)",
    ));
    let Some((conclusion, events, _)) =
      run_patched(step, &[("post.js", "process.exit(3);\n")]).await?
    else {
      return Ok(());
    };
    // The post takes the name the step reported after its main-stage update.
    assert_eq!(reported(&events, "Post Deferred "), Some(expected));
    let post = events.iter().rev().find_map(|event| {
      if let RunnerEvent::StepCompleted { conclusion, .. } = event {
        Some(*conclusion)
      } else {
        None
      }
    });
    assert_eq!(
      post,
      Some(expected),
      "post conclusion, continue-on-error={continue_on_error}"
    );
    assert_eq!(
      conclusion, expected,
      "job conclusion, continue-on-error={continue_on_error}"
    );
  }
  Ok(())
}

/// 3c: a failing `pre` applies the step's `continue-on-error` as its own
/// step. Without it the failure becomes the step's conclusion and `main`
/// does not run; with it, `pre` concludes `success` and `main` runs. Either
/// way the `post` runs, as upstream registers it before running `pre`.
#[tokio::test]
async fn failing_pre_honors_continue_on_error() -> TestResult<()> {
  for (continue_on_error, expected, ran_main) in [
    (true, shared::Conclusion::Success, true),
    (false, shared::Conclusion::Failure, false),
  ] {
    let mut step = action_step("a", "act-a");
    step.set_continue_on_error(continue_on_error);
    let Some((conclusion, events, markers)) =
      run_patched(step, &[("pre.js", "process.exit(2);\n")]).await?
    else {
      return Ok(());
    };
    assert_eq!(reported(&events, "Pre Run ./act-a"), Some(expected));
    assert_eq!(
      conclusion, expected,
      "continue-on-error={continue_on_error}"
    );
    assert_eq!(
      markers.iter().any(|m| m == "A:main"),
      ran_main,
      "{markers:?}"
    );
    // Upstream registers the post before running `pre`, so it runs either way.
    assert!(
      markers.iter().any(|m| m.starts_with("A:post:")),
      "{markers:?}"
    );
  }
  Ok(())
}

/// 3d: `pre` and `main` each get the step's full `timeout-minutes` budget,
/// like upstream's separate pre step: two 35 s stages both finish under
/// `timeout-minutes: 1` even though together they exceed it.
#[tokio::test]
async fn pre_and_main_get_separate_timeout_budgets() -> TestResult<()> {
  let mut step = action_step("a", "act-a");
  step.timeout_in_minutes = Some(shared::TemplateToken::number(1.0));
  let stage = |name: &str| {
    format!(
      "setTimeout(() => require('fs').appendFileSync(process.env.MARKER_FILE, '{name}-done\\n'), 35000);\n"
    )
  };
  let Some((conclusion, _events, markers)) = run_patched(
    step,
    &[("pre.js", &stage("pre")), ("main.js", &stage("main"))],
  )
  .await?
  else {
    return Ok(());
  };
  assert_eq!(conclusion, shared::Conclusion::Success, "{markers:?}");
  assert!(markers.iter().any(|m| m == "pre-done"), "{markers:?}");
  assert!(markers.iter().any(|m| m == "main-done"), "{markers:?}");
  Ok(())
}

/// 3e: a `pre` that outlives its budget is killed and reported under its own
/// `Pre` name, like upstream's separate pre step; `main` never runs. The pre
/// keeps the job-start name, where `steps.*` is still unevaluated, while the
/// step itself is renamed with its live contexts.
#[tokio::test]
async fn pre_timeout_reports_the_pre_step() -> TestResult<()> {
  let mut step = action_step("a", "act-a");
  step.timeout_in_minutes = Some(shared::TemplateToken::number(1.0));
  step.display_name_token = Some(shared::TemplateToken::expression(
    "format('Deferred {0}', steps.prior.outputs.label)",
  ));
  let Some((conclusion, events, markers)) =
    run_patched(step, &[("pre.js", "setTimeout(() => {}, 90000);\n")]).await?
  else {
    return Ok(());
  };
  assert_eq!(conclusion, shared::Conclusion::Failure, "{markers:?}");
  let pre = "Pre Deferred ${{ steps.prior.outputs.label }}";
  assert_eq!(reported(&events, pre), Some(shared::Conclusion::Failure));
  // The failed pre fails the job, so the `success()` main is skipped (#251).
  assert_eq!(
    reported(&events, "Deferred "),
    Some(shared::Conclusion::Skipped)
  );
  let timeouts: Vec<&str> = events
    .iter()
    .filter_map(|event| {
      if let RunnerEvent::Log { line, .. } = event
        && line.contains("timed out")
      {
        Some(line.as_str())
      } else {
        None
      }
    })
    .collect();
  assert_eq!(
    timeouts,
    [format!(
      "##[error]The action '{pre}' has timed out after 1 minutes."
    )]
  );
  assert!(!markers.iter().any(|m| m == "A:main"), "{markers:?}");
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

/// `drive`, but returning the raw `run_steps` `Result` so error-policy tests
/// can distinguish a step failure conclusion from an escaped engine error.
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
  let http = reqwest::Client::new();
  let fetcher =
    execution::execution::actions::prefetch::ActionFetcher::new(&shared::NetworkConfig::default())?;
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
      http: &http,
      fetcher: &fetcher,
    },
  )
  .await;
  drop(tx);
  collector.await?;
  Ok(result)
}

/// Common single-action rig shared by the action-error and cancel-grace tests
/// below: a temp workspace/data dir with the real node runtime + `act-a`
/// (marker `"A"`) seeded, and an empty marker file. Returns the `TempDir`
/// guard (keep it alive for the test's duration) alongside the paths + config.
fn seed_single_action_fixture(
  node: &Path,
) -> TestResult<(tempfile::TempDir, PathBuf, RunnerConfig, PathBuf)> {
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
  seed_node(&data_dir, node)?;
  install_action(&workspace, "act-a", "A")?;
  let marker_file = data_dir.join("markers.txt");
  std::fs::write(&marker_file, "")?;
  Ok((dir, workspace, config, marker_file))
}

/// 3b: a later action resolution error becomes a step Failure and posts still
/// drain with their saved state before `run_steps` returns the job conclusion.
#[tokio::test]
async fn post_drains_even_when_a_later_step_errors_hard() -> TestResult<()> {
  let Some(node) = system_node() else {
    eprintln!("SKIP: no system `node` on PATH; pre/post test needs a real node runtime");
    return Ok(());
  };

  let (_dir, workspace, config, marker_file) = seed_single_action_fixture(&node)?;

  // Step 2 references a local action dir that does not exist, producing a real
  // resolution error that the step loop must normalize to Failure.
  let mut broken = ActionStep::with_ref_type("broken", "repository");
  broken.reference.name = Some("./does-not-exist".to_owned());
  broken.reference.git_ref = None;
  let steps = vec![action_step("a", "act-a"), broken];

  let result = drive_raw(&steps, &workspace, &config, &marker_file).await?;
  assert!(
    matches!(result, Ok(shared::Conclusion::Failure)),
    "an unresolvable action must produce a Failure conclusion; got {result:?}"
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

  let (_dir, workspace, config, marker_file) = seed_single_action_fixture(&node)?;
  // Swap the local main for the slow variant AFTER installing; pre/post stay real.
  std::fs::write(workspace.join("act-a/main.js"), SLOW_MAIN_JS)?;

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

/// Every `StepMetadata` `(type, action, ref)` reported for the row `name`.
fn metadata(events: &[RunnerEvent], name: &str) -> Vec<(String, Option<String>, Option<String>)> {
  let id = events.iter().find_map(|event| {
    if let RunnerEvent::StepStarted {
      step_id, step_name, ..
    } = event
      && step_name == name
    {
      Some(step_id)
    } else {
      None
    }
  });
  events
    .iter()
    .filter_map(|event| {
      if let RunnerEvent::StepMetadata {
        step_id,
        kind,
        action,
        git_ref,
      } = event
        && Some(step_id) == id
      {
        Some((kind.clone(), action.clone(), git_ref.clone()))
      } else {
        None
      }
    })
    .collect()
}

/// Issue 88: the pre, main and post rows of a Node action each carry one
/// `node20` + path label (upstream `Handler.PopulateActionTelemetry`); a post
/// skipped by `post-if` is reported without one, since no handler ran.
#[tokio::test]
async fn pre_main_and_post_rows_carry_action_metadata_and_a_skipped_post_none() -> TestResult<()> {
  let manifest = std::fs::read_to_string(Path::new(FIXTURE_DIR).join("action.yml"))?;
  let label = vec![("node20".to_owned(), Some("./act-a".to_owned()), None)];
  for (post_if, post_label) in [(None, label.clone()), (Some("failure()"), Vec::new())] {
    let action = match post_if {
      Some(condition) => format!("{manifest}  post-if: '{condition}'\n"),
      None => manifest.clone(),
    };
    let Some((conclusion, events, _)) =
      run_patched(action_step("a", "act-a"), &[("action.yml", &action)]).await?
    else {
      return Ok(());
    };
    assert_eq!(conclusion, shared::Conclusion::Success);
    assert_eq!(metadata(&events, "Pre Run ./act-a"), label);
    assert_eq!(metadata(&events, "Run ./act-a"), label);
    let post = "Post Run ./act-a";
    let expected = if post_if.is_some() {
      shared::Conclusion::Skipped
    } else {
      shared::Conclusion::Success
    };
    assert_eq!(reported(&events, post), Some(expected), "{post_if:?}");
    assert_eq!(metadata(&events, post), post_label, "{post_if:?}");
  }
  Ok(())
}
