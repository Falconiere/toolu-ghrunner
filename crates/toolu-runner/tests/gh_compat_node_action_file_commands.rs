//! Live-bug regression: a Node.js action's `$GITHUB_PATH`/`$GITHUB_ENV` file
//! commands must reach every LATER step, exactly like a `run:` step's do.
//!
//! Before the fix, `execution::execution::node_stage::run_node_stage` never
//! created `$GITHUB_ENV`/`$GITHUB_OUTPUT`/`$GITHUB_PATH` file-command files
//! for a Node.js action stage, so `@actions/core`'s `addPath`/`exportVariable`
//! (used by real actions like `oven-sh/setup-bun`/`actions/setup-node`) fell
//! back to the stdout `::add-path::`/`::set-env::` commands — which
//! `CommandDispatcher` refuses outright (CVE-2020-15228) — so the export
//! silently had no effect. A `run:` step right after such an action could not
//! see the PATH entry or env var the action had "successfully" exported,
//! reproducing the real failure: `bun: command not found` after a green
//! `setup-bun` step.
//!
//! Real-data, no mocks. Drives a committed node-action fixture
//! (`fixtures/path_env_action/main.js`, real `@actions/core`-style
//! `fs.appendFileSync(process.env.GITHUB_PATH, …)`/`GITHUB_ENV`) through the
//! live step loop (`execution::steps_runner::run_steps`), then a real `run:`
//! step that resolves a real executable placed ONLY in the exported PATH
//! entry (bare `fixture-tool`, not a full path — exactly how `bun`/`node`
//! commands are invoked by later steps) and reads the exported env var. The
//! action's real `node` binary and manifest are seeded on disk (no network,
//! fully hermetic); the test skips if no system `node` is available.
//!
//! `main.js` itself guards the contract: it throws a named
//! `FIXTURE CONTRACT VIOLATION` error if `$GITHUB_PATH`/`$GITHUB_ENV` are
//! ever unset or unwritable, so a regression that deletes the runner's
//! file-command wiring fails LOUDLY at the mechanism (a named stderr `Log`
//! line this test asserts is absent) rather than incidentally, downstream,
//! as a bare `fixture-tool: command not found`.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use execution::execution::actions::downloader::{action_cache_dir, watermark_path};
use execution::execution::context::ExecutionContext;
use execution::execution::steps_runner::{JobRun, run_steps};
use execution::node::runtime::{node_binary_path, node_cache_dir, node_version_for};
use shared::SecretMasker;
use shared::{ActionStep, RunnerConfig, RunnerEvent};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

type TestResult<T> = Result<T, Box<dyn Error>>;

const FIXTURE_DIR: &str = concat!(
  env!("CARGO_MANIFEST_DIR"),
  "/tests/fixtures/path_env_action"
);

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
/// `ensure_node_runtime` resolves it from disk instead of downloading.
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
/// `toolu/path-env-action/v1`, with the `bin_dir` input default rewritten to
/// `bin_dir` (a real, empty-at-seed-time directory the test controls).
fn seed_action(data_dir: &Path, bin_dir: &Path) -> TestResult<()> {
  let cache_key = "toolu/path-env-action/v1";
  let cache_dir = action_cache_dir(data_dir, cache_key);
  std::fs::create_dir_all(&cache_dir)?;

  for name in ["action.yml", "main.js"] {
    let src = Path::new(FIXTURE_DIR).join(name);
    let contents = std::fs::read_to_string(&src)?;
    let patched = if name == "action.yml" {
      contents.replace("/__PLACEHOLDER_BIN_DIR__", &bin_dir.to_string_lossy())
    } else {
      contents
    };
    std::fs::write(cache_dir.join(name), patched)?;
  }

  std::fs::write(watermark_path(&cache_dir), "")?;
  Ok(())
}

/// Build the node-action step (`uses: toolu/path-env-action@v1`).
fn action_step(id: &str) -> ActionStep {
  let mut step = ActionStep::with_ref_type(id, "repository");
  step.reference.name = Some("toolu/path-env-action".to_owned());
  step.reference.git_ref = Some("v1".to_owned());
  step
}

/// Write a real, executable "tool" into `bin_dir` — nothing else on `PATH`
/// can produce its output, so a later step resolving it BARE (`fixture-tool`,
/// not a full path) proves the exported PATH entry actually reached that step.
#[cfg(unix)]
fn write_fixture_tool(bin_dir: &Path) -> TestResult<()> {
  use std::os::unix::fs::PermissionsExt;
  std::fs::create_dir_all(bin_dir)?;
  let tool = bin_dir.join("fixture-tool");
  std::fs::write(&tool, "#!/bin/sh\necho fixture-tool-ran\n")?;
  let mut perms = std::fs::metadata(&tool)?.permissions();
  perms.set_mode(0o755);
  std::fs::set_permissions(&tool, perms)?;
  Ok(())
}

/// Drive `steps` through the live step loop with a pre-seeded action+node
/// cache, returning the run-step's captured stdout `Log` lines.
async fn drive(
  steps: &[ActionStep],
  workspace: &Path,
  config: &RunnerConfig,
) -> TestResult<Vec<String>> {
  let masker = Arc::new(Mutex::new(SecretMasker::new()));
  let mut ctx = ExecutionContext::with_masker(Arc::clone(&masker));

  let (tx, mut rx) = mpsc::channel::<RunnerEvent>(1024);
  let collector = tokio::spawn(async move {
    let mut lines = Vec::new();
    while let Some(event) = rx.recv().await {
      if let RunnerEvent::Log { line, .. } = event {
        lines.push(line);
      }
    }
    lines
  });

  let spec = execution::execution::job_spec::JobSpec::default();
  let http = reqwest::Client::new();
  let conclusion = run_steps(
    steps,
    &mut ctx,
    &tx,
    CancellationToken::new(),
    &JobRun {
      workspace,
      config,
      spec: &spec,
      shadow: None,
      http: &http,
    },
  )
  .await?;
  drop(tx);
  let lines = collector.await?;
  assert_eq!(
    conclusion,
    shared::Conclusion::Success,
    "job must succeed; log lines: {lines:?}"
  );
  Ok(lines)
}

/// The live bug: a node action's `$GITHUB_PATH`/`$GITHUB_ENV` exports must be
/// visible to a LATER `run:` step — both the bare-command PATH resolution
/// (the exact `bun: command not found` repro) and a plain env var, including
/// the heredoc `KEY<<DELIM` form.
#[tokio::test]
async fn node_action_path_and_env_exports_reach_the_next_step() -> TestResult<()> {
  let Some(node) = system_node() else {
    eprintln!("SKIP: no system `node` on PATH; this test needs a real node runtime");
    return Ok(());
  };

  let dir = tempfile::tempdir()?;
  let (workspace, data_dir) = (dir.path().join("work"), dir.path().join("data"));
  std::fs::create_dir_all(&workspace)?;
  std::fs::create_dir_all(&data_dir)?;
  let bin_dir = data_dir.join("fixture-bin");
  write_fixture_tool(&bin_dir)?;

  let config = RunnerConfig {
    data_dir: data_dir.clone(),
    workspace_root: workspace.clone(),
    cgroup_path: None,
    services_mode: shared::ServicesMode::default(),
    ..RunnerConfig::default()
  };

  seed_node(&data_dir, &node)?;
  seed_action(&data_dir, &bin_dir)?;

  let mut verify = ActionStep::script(
    "verify",
    "fixture-tool\n\
     echo \"GREETING=$FIXTURE_GREETING\"\n\
     echo \"MULTILINE=$FIXTURE_MULTILINE\"",
    "",
  );
  verify.continue_on_error = Some(false);
  let steps = vec![action_step("setup"), verify];

  let lines = drive(&steps, &workspace, &config).await?;

  // The fixture's `main.js` throws a named `FIXTURE CONTRACT VIOLATION`
  // error (streamed here as a stderr `Log` line) if the runner ever fails to
  // inject a writable $GITHUB_PATH/$GITHUB_ENV for this stage. Asserting its
  // absence proves the guard actually ran and passed on the live wiring — if
  // `run_node_stage`'s file-command injection regresses, THIS assertion
  // fails first, naming the missing contract, instead of the job merely
  // failing incidentally later at `fixture-tool: command not found`.
  assert!(
    !lines
      .iter()
      .any(|l| l.contains("FIXTURE CONTRACT VIOLATION")),
    "the node action must not hit its file-command contract guard when the \
     runner's $GITHUB_PATH/$GITHUB_ENV wiring is present; got lines: {lines:?}"
  );
  assert!(
    lines.iter().any(|l| l == "fixture-tool-ran"),
    "the exported $GITHUB_PATH entry must resolve a BARE command in the next \
     step (live bug repro: PATH additions from a node action never reached \
     later steps); got lines: {lines:?}"
  );
  assert!(
    lines.iter().any(|l| l == "GREETING=hello-from-action"),
    "the exported $GITHUB_ENV var must be visible to the next step; got lines: {lines:?}"
  );
  assert!(
    lines.iter().any(|l| l == "MULTILINE=line one"),
    "the heredoc-form $GITHUB_ENV var must be visible to the next step \
     (only the first line survives an unquoted $VAR echo, which is enough to \
     prove it propagated); got lines: {lines:?}"
  );
  Ok(())
}
