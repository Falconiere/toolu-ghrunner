//! B-004: a composite action's own inline `run:` step must interpolate
//! `${{ runner.temp }}` to the SAME value it sets as its own `$RUNNER_TEMP`.
//! `composite_exec.rs` used to source the two from different paths (the
//! interpolation from `data_dir/tmp`, the env from `data_dir/_temp` via
//! `composite_env.rs`); this drives the real public `execute_composite_action`
//! entry point with a real bash subprocess (no mocks) to pin the fix.
//!
//! Both composite interpolation sites are covered: the `run:` script body
//! (`composite_exec::run_run_step`) and the step's own `env:` values
//! (`composite_env::build_step_env`) — they are distinct code paths and
//! either alone could regress.

use std::collections::HashMap;
use std::error::Error;
use std::path::{Path, PathBuf};

use execution::execution::actions::manifest::{
  ActionDefinition, ActionRuns, CompositeStep, RunsUsing,
};
use execution::execution::composite_exec::{CompositeParams, execute_composite_action};
use execution::execution::context::ExecutionContext;
use execution::execution::depth_tracker::DepthTracker;
use shared::{Conclusion, RunnerConfig, RunnerEvent};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

type TestResult = Result<(), Box<dyn Error>>;

/// The literal composite-expression text (kept out of any Rust format
/// string so it needs no brace-escaping).
const RUNNER_TEMP_EXPR: &str = "${{ runner.temp }}";

/// Owned fixtures for one `execute_composite_action` run, so the test
/// function itself only wires them up and asserts.
struct Fixture {
  data_dir: tempfile::TempDir,
  probe_dir: PathBuf,
  workspace: PathBuf,
  config: RunnerConfig,
  manifest: ActionDefinition,
  step_inputs: HashMap<String, String>,
  events: mpsc::Sender<RunnerEvent>,
  cancel: CancellationToken,
  http: reqwest::Client,
}

impl Fixture {
  fn new() -> Result<Self, Box<dyn Error>> {
    let data_dir = tempfile::tempdir()?;
    let workspace = data_dir.path().join("_work");
    std::fs::create_dir_all(&workspace)?;
    let probe_dir = data_dir.path().join("probe");
    std::fs::create_dir_all(&probe_dir)?;
    let config = RunnerConfig {
      data_dir: data_dir.path().to_path_buf(),
      workspace_root: workspace.clone(),
      ..RunnerConfig::default()
    };
    let manifest = probe_manifest(&probe_dir);
    let (events, mut rx) = mpsc::channel(64);
    // Drain in the background so `execute_composite_action`'s log sends
    // never block on a full buffer.
    tokio::spawn(async move { while rx.recv().await.is_some() {} });

    Ok(Self {
      data_dir,
      probe_dir,
      workspace,
      config,
      manifest,
      step_inputs: HashMap::new(),
      events,
      cancel: CancellationToken::new(),
      http: reqwest::Client::new(),
    })
  }

  fn params(&self) -> CompositeParams<'_> {
    CompositeParams {
      manifest: &self.manifest,
      step_inputs: &self.step_inputs,
      events: &self.events,
      workspace: &self.workspace,
      config: &self.config,
      parent_step_id: "parent",
      action_dir: self.data_dir.path(),
      cancel: &self.cancel,
      http: &self.http,
    }
  }
}

/// A one-step composite manifest whose `run:` step writes three probes under
/// `probe_dir`: its interpolated `${{ runner.temp }}` (script-body site), its
/// own `$RUNNER_TEMP` env, and `$MY_TEMP` — a step `env:` value carrying the
/// same expression (the `build_step_env` interpolation site).
fn probe_manifest(probe_dir: &Path) -> ActionDefinition {
  let interp_file = probe_dir.join("interp.txt").to_string_lossy().into_owned();
  let env_file = probe_dir.join("env.txt").to_string_lossy().into_owned();
  let env_interp_file = probe_dir
    .join("env_interp.txt")
    .to_string_lossy()
    .into_owned();
  let script = format!(
    "echo -n '{RUNNER_TEMP_EXPR}' > '{interp_file}'\necho -n \"$RUNNER_TEMP\" > '{env_file}'\necho -n \"$MY_TEMP\" > '{env_interp_file}'\n"
  );

  ActionDefinition {
    name: "probe".to_owned(),
    description: String::new(),
    inputs: HashMap::new(),
    outputs: HashMap::new(),
    runs: ActionRuns {
      using: RunsUsing::Composite,
      main: None,
      pre: None,
      post: None,
      pre_if: None,
      post_if: None,
      image: None,
      steps: vec![CompositeStep {
        id: Some("probe".to_owned()),
        name: Some("probe runner.temp".to_owned()),
        run: Some(script),
        shell: Some("bash".to_owned()),
        env: HashMap::from([("MY_TEMP".to_owned(), RUNNER_TEMP_EXPR.to_owned())]),
        condition: None,
        uses: None,
        continue_on_error: false,
        with: HashMap::new(),
      }],
    },
  }
}

#[tokio::test]
async fn composite_run_step_interpolated_runner_temp_matches_its_own_env() -> TestResult {
  let fixture = Fixture::new()?;
  let params = fixture.params();
  let mut ctx = ExecutionContext::new_for_test();
  // Mirrors job start (`job_runner::build_context`): populates the
  // `runner.*` context that composite interpolation now reads back
  // through (B-005), same as it always populated $RUNNER_TEMP/$RUNNER_TOOL_CACHE.
  ctx.set_runner_context("test-runner", fixture.data_dir.path())?;
  let mut depth = DepthTracker::new();

  let result = execute_composite_action(&params, &mut ctx, &mut depth).await?;
  assert_eq!(
    result.conclusion,
    Conclusion::Success,
    "probe composite step must succeed"
  );

  let interpolated = std::fs::read_to_string(fixture.probe_dir.join("interp.txt"))?;
  let env_value = std::fs::read_to_string(fixture.probe_dir.join("env.txt"))?;
  let expected = fixture
    .data_dir
    .path()
    .join("_temp")
    .to_string_lossy()
    .into_owned();

  assert_eq!(
    interpolated, expected,
    "the composite step's own ${{{{ runner.temp }}}} interpolation must be data_dir/_temp"
  );
  assert_eq!(
    env_value, expected,
    "the composite step's own $RUNNER_TEMP env must be data_dir/_temp"
  );
  assert_eq!(
    interpolated, env_value,
    "B-004: interpolated runner.temp and env RUNNER_TEMP must be identical"
  );

  let env_interpolated = std::fs::read_to_string(fixture.probe_dir.join("env_interp.txt"))?;
  assert_eq!(
    env_interpolated, expected,
    "a step `env:` value interpolating runner.temp (build_step_env site) must also be data_dir/_temp"
  );
  Ok(())
}
