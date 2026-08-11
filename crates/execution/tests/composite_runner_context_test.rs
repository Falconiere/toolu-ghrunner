//! B-005: composite interpolation must answer the real `runner.*` fields
//! identically to the real `${{ }}` evaluator, not fall back to `""` for
//! anything beyond `os`/`arch`/`temp`. Drives the real public
//! `execute_composite_action` entry point with a real bash subprocess (no
//! mocks), mirroring `composite_runner_temp_test.rs`'s fixture shape (which
//! pins `runner.temp`).
//!
//! - `tool_cache_interpolates_in_run_body_and_step_env` pins `runner.tool_cache`
//!   at both interpolation sites (the `run:` script body and a step `env:`
//!   value) to `data_dir/_tool` — the same value `set_runner_context`
//!   establishes and the real evaluator answers (`gh_compat_context.rs`).
//! - `documented_fields_match_set_runner_context_and_unknown_field_is_empty`
//!   pins every other field `set_runner_context` populates (`os`/`arch`/
//!   `name`) to that same source, and confirms a genuinely unknown field
//!   still resolves to `""` (unchanged upstream semantics).

use std::collections::HashMap;
use std::error::Error;
use std::path::{Path, PathBuf};

use execution::execution::actions::manifest::{
  ActionDefinition, ActionRuns, CompositeStep, RunsUsing,
};
use execution::execution::actions::prefetch::ActionFetcher;
use execution::execution::composite_exec::{CompositeParams, execute_composite_action};
use execution::execution::context::ExecutionContext;
use execution::execution::depth_tracker::DepthTracker;
use shared::platform::{runner_arch, runner_os};
use shared::{Conclusion, RunnerConfig, RunnerEvent};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

type TestResult = Result<(), Box<dyn Error>>;

/// The runner name every fixture calls `set_runner_context` with — what
/// `${{ runner.name }}` must answer.
const RUNNER_NAME: &str = "composite-probe-runner";

/// The literal composite-expression text (kept out of any Rust format
/// string so it needs no brace-escaping).
const TOOL_CACHE_EXPR: &str = "${{ runner.tool_cache }}";
const OS_EXPR: &str = "${{ runner.os }}";
const ARCH_EXPR: &str = "${{ runner.arch }}";
const NAME_EXPR: &str = "${{ runner.name }}";
const BOGUS_EXPR: &str = "${{ runner.bogus }}";

/// Owned fixtures for one `execute_composite_action` run, so each test
/// function only supplies its probe step and asserts on the results.
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
  fetcher: ActionFetcher,
}

impl Fixture {
  /// `build_step` receives the (already-created) probe dir and returns the
  /// composite step to run.
  fn new(build_step: impl FnOnce(&Path) -> CompositeStep) -> Result<Self, Box<dyn Error>> {
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
    let manifest = probe_manifest(vec![build_step(&probe_dir)]);
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
      fetcher: ActionFetcher::new(),
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
      fetcher: &self.fetcher,
    }
  }

  /// Populate `runner.*` (mirrors job start: `job_runner::build_context`
  /// calling `set_runner_context`, the same population composite
  /// interpolation now reads back through, B-005), then run the probe
  /// composite action through it.
  async fn run(&self) -> Result<Conclusion, Box<dyn Error>> {
    let mut ctx = ExecutionContext::new_for_test();
    ctx.set_runner_context(RUNNER_NAME, self.data_dir.path())?;
    let mut depth = DepthTracker::new();
    let params = self.params();
    let result = execute_composite_action(&params, &mut ctx, &mut depth).await?;
    Ok(result.conclusion)
  }

  fn read_probe(&self, file_name: &str) -> Result<String, Box<dyn Error>> {
    Ok(std::fs::read_to_string(self.probe_dir.join(file_name))?)
  }

  fn expected_tool_cache(&self) -> String {
    self
      .data_dir
      .path()
      .join("_tool")
      .to_string_lossy()
      .into_owned()
  }
}

/// `steps` is a `Vec` (not a bare `CompositeStep`) so this stays a
/// small-by-value parameter regardless of the step's own field count.
fn probe_manifest(steps: Vec<CompositeStep>) -> ActionDefinition {
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
      steps,
    },
  }
}

#[tokio::test]
async fn tool_cache_interpolates_in_run_body_and_step_env() -> TestResult {
  let fixture = Fixture::new(|probe_dir| {
    let body_file = probe_dir.join("body.txt").to_string_lossy().into_owned();
    let env_file = probe_dir.join("env.txt").to_string_lossy().into_owned();
    let script = format!(
      "echo -n '{TOOL_CACHE_EXPR}' > '{body_file}'\necho -n \"$MY_TOOL_CACHE\" > '{env_file}'\n"
    );
    CompositeStep {
      id: Some("probe".to_owned()),
      name: Some("probe runner.tool_cache".to_owned()),
      run: Some(script),
      shell: Some("bash".to_owned()),
      env: HashMap::from([("MY_TOOL_CACHE".to_owned(), TOOL_CACHE_EXPR.to_owned())]),
      condition: None,
      uses: None,
      continue_on_error: false,
      with: HashMap::new(),
    }
  })?;

  let conclusion = fixture.run().await?;
  assert_eq!(
    conclusion,
    Conclusion::Success,
    "probe composite step must succeed"
  );

  let expected = fixture.expected_tool_cache();
  let body = fixture.read_probe("body.txt")?;
  let env = fixture.read_probe("env.txt")?;
  assert_eq!(
    body, expected,
    "a composite run: body's runner.tool_cache interpolation must be data_dir/_tool"
  );
  assert_eq!(
    env, expected,
    "a composite step env: value interpolating runner.tool_cache must be data_dir/_tool"
  );
  Ok(())
}

#[tokio::test]
async fn documented_fields_match_set_runner_context_and_unknown_field_is_empty() -> TestResult {
  let fixture = Fixture::new(|probe_dir| {
    let os_file = probe_dir.join("os.txt").to_string_lossy().into_owned();
    let arch_file = probe_dir.join("arch.txt").to_string_lossy().into_owned();
    let name_file = probe_dir.join("name.txt").to_string_lossy().into_owned();
    let bogus_file = probe_dir.join("bogus.txt").to_string_lossy().into_owned();
    let script = format!(
      "echo -n '{OS_EXPR}' > '{os_file}'\necho -n '{ARCH_EXPR}' > '{arch_file}'\necho -n '{NAME_EXPR}' > '{name_file}'\necho -n '{BOGUS_EXPR}' > '{bogus_file}'\n"
    );
    CompositeStep {
      id: Some("probe".to_owned()),
      name: Some("probe runner.* fields".to_owned()),
      run: Some(script),
      shell: Some("bash".to_owned()),
      env: HashMap::new(),
      condition: None,
      uses: None,
      continue_on_error: false,
      with: HashMap::new(),
    }
  })?;

  let conclusion = fixture.run().await?;
  assert_eq!(
    conclusion,
    Conclusion::Success,
    "probe composite step must succeed"
  );

  assert_eq!(
    fixture.read_probe("os.txt")?,
    runner_os(),
    "runner.os must match the host-derived value set_runner_context sets"
  );
  assert_eq!(
    fixture.read_probe("arch.txt")?,
    runner_arch(),
    "runner.arch must match the host-derived value set_runner_context sets"
  );
  assert_eq!(
    fixture.read_probe("name.txt")?,
    RUNNER_NAME,
    "runner.name must match the name set_runner_context was called with"
  );
  assert_eq!(
    fixture.read_probe("bogus.txt")?,
    "",
    "a genuinely unknown runner.* field must still resolve to an empty string"
  );
  Ok(())
}
