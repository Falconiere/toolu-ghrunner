use std::collections::HashMap;

use serde::Deserialize;
use shared::RunnerError;

/// Parsed action definition from action.yml/action.yaml.
#[derive(Debug, Clone)]
pub struct ActionDefinition {
  /// The action's display name.
  pub name: String,
  /// The action's description.
  pub description: String,
  /// Declared `inputs:` keyed by input name.
  pub inputs: HashMap<String, ActionInput>,
  /// Declared `outputs:` keyed by output name.
  pub outputs: HashMap<String, ActionOutput>,
  /// The `runs:` section (execution kind + entrypoints).
  pub runs: ActionRuns,
}

/// An input parameter for an action.
#[derive(Debug, Clone)]
pub struct ActionInput {
  /// Human-readable description of the input.
  pub description: String,
  /// Whether the step must supply this input.
  pub required: bool,
  /// Default value used when the step doesn't set this input.
  pub default: Option<String>,
}

/// An output parameter for an action.
#[derive(Debug, Clone)]
pub struct ActionOutput {
  /// Human-readable description of the output.
  pub description: String,
  /// Expression producing the output's value (composite actions).
  pub value: Option<String>,
}

/// A single step within a composite action's `steps:` array.
#[derive(Debug, Clone)]
pub struct CompositeStep {
  /// Step id, used to reference its outputs from other steps.
  pub id: Option<String>,
  /// Display name for the step.
  pub name: Option<String>,
  /// Shell script body, for a `run:` step.
  pub run: Option<String>,
  /// Shell to run the script under.
  pub shell: Option<String>,
  /// `KEY=VALUE` environment entries for the step.
  pub env: HashMap<String, String>,
  /// `if:` expression gating whether the step runs.
  pub condition: Option<String>,
  /// Nested action reference, for a `uses:` step.
  pub uses: Option<String>,
  /// Whether a failure in this step should not fail the job.
  pub continue_on_error: bool,
  /// `with:` inputs passed to a nested `uses:` step.
  pub with: HashMap<String, String>,
}

/// The `runs` section of an action manifest.
#[derive(Debug, Clone)]
pub struct ActionRuns {
  /// Execution kind (`node`, `composite`, `docker`).
  pub using: RunsUsing,
  /// Main entrypoint script/executable.
  pub main: Option<String>,
  /// Pre-step entrypoint, run before the main step.
  pub pre: Option<String>,
  /// Post-step entrypoint, run after the job's steps complete.
  pub post: Option<String>,
  /// `if:` expression gating the pre-step.
  pub pre_if: Option<String>,
  /// `if:` expression gating the post-step.
  pub post_if: Option<String>,
  /// Docker image reference, for a docker action.
  pub image: Option<String>,
  /// The composite action's `steps:` array.
  pub steps: Vec<CompositeStep>,
}

/// The runtime type from `runs.using`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunsUsing {
  /// Node.js action — major version from `runs.using` (e.g. 20, 24).
  Node {
    /// Node.js major version to run the action under.
    major: u8,
  },
  /// Composite action — runs a nested `steps:` array.
  Composite,
  /// Docker action — runs a container image.
  Docker,
}

/// Parse an action.yml/action.yaml string into an `ActionDefinition`.
///
/// # Errors
///
/// Returns `RunnerError::ActionManifest` on YAML parse errors or missing fields.
pub fn parse_action_manifest(yaml_content: &str) -> Result<ActionDefinition, RunnerError> {
  let raw: RawActionManifest = serde_yaml::from_str(yaml_content)
    .map_err(|e| RunnerError::ActionManifest(format!("parse action.yml: {e}")))?;

  let runs_raw = raw
    .runs
    .ok_or_else(|| RunnerError::ActionManifest("missing 'runs' section".to_owned()))?;

  let using = parse_runs_using(&runs_raw.using)?;

  let inputs = parse_inputs(raw.inputs);
  let outputs = parse_outputs(raw.outputs);
  let steps = parse_composite_steps(runs_raw.steps.as_ref());

  Ok(ActionDefinition {
    name: raw.name.unwrap_or_default(),
    description: raw.description.unwrap_or_default(),
    inputs,
    outputs,
    runs: ActionRuns {
      using,
      main: runs_raw.main,
      pre: runs_raw.pre,
      post: runs_raw.post,
      pre_if: runs_raw.pre_if,
      post_if: runs_raw.post_if,
      image: runs_raw.image,
      steps,
    },
  })
}

fn parse_inputs(raw_inputs: Option<HashMap<String, RawInput>>) -> HashMap<String, ActionInput> {
  raw_inputs
    .unwrap_or_default()
    .into_iter()
    .map(|(k, v)| {
      (
        k,
        ActionInput {
          description: v.description.unwrap_or_default(),
          required: v.required.unwrap_or(false),
          default: v.default,
        },
      )
    })
    .collect()
}

fn parse_outputs(raw_outputs: Option<HashMap<String, RawOutput>>) -> HashMap<String, ActionOutput> {
  raw_outputs
    .unwrap_or_default()
    .into_iter()
    .map(|(k, v)| {
      (
        k,
        ActionOutput {
          description: v.description.unwrap_or_default(),
          value: v.value,
        },
      )
    })
    .collect()
}

fn parse_runs_using(using: &str) -> Result<RunsUsing, RunnerError> {
  if let Some(digits) = using.strip_prefix("node") {
    let major: u8 = digits
      .parse()
      .map_err(|e| RunnerError::ActionManifest(format!("invalid node version '{using}': {e}")))?;
    // Deprecated versions (node12, node16) redirect to node20.
    if major < 20 {
      tracing::warn!(using, "deprecated node version, redirecting to node20");
      return Ok(RunsUsing::Node { major: 20 });
    }
    return Ok(RunsUsing::Node { major });
  }
  match using {
    "composite" => Ok(RunsUsing::Composite),
    "docker" => Ok(RunsUsing::Docker),
    other => Err(RunnerError::ActionManifest(format!(
      "unsupported runs.using: '{other}'"
    ))),
  }
}

#[derive(Deserialize)]
struct RawActionManifest {
  name: Option<String>,
  description: Option<String>,
  inputs: Option<HashMap<String, RawInput>>,
  outputs: Option<HashMap<String, RawOutput>>,
  runs: Option<RawRuns>,
}

#[derive(Deserialize)]
struct RawInput {
  description: Option<String>,
  required: Option<bool>,
  default: Option<String>,
}

#[derive(Deserialize)]
struct RawOutput {
  description: Option<String>,
  value: Option<String>,
}

#[derive(Deserialize)]
struct RawRuns {
  using: String,
  main: Option<String>,
  pre: Option<String>,
  post: Option<String>,
  #[serde(rename = "pre-if")]
  pre_if: Option<String>,
  #[serde(rename = "post-if")]
  post_if: Option<String>,
  image: Option<String>,
  #[serde(default)]
  steps: Option<Vec<RawCompositeStep>>,
}

#[derive(Deserialize, Default)]
struct RawCompositeStep {
  id: Option<String>,
  name: Option<String>,
  run: Option<String>,
  shell: Option<String>,
  #[serde(default)]
  env: HashMap<String, String>,
  #[serde(rename = "if")]
  condition: Option<String>,
  uses: Option<String>,
  #[serde(default)]
  continue_on_error: bool,
  #[serde(default)]
  with: HashMap<String, String>,
}

fn parse_composite_steps(raw: Option<&Vec<RawCompositeStep>>) -> Vec<CompositeStep> {
  let Some(steps) = raw else {
    return Vec::new();
  };
  steps
    .iter()
    .map(|s| CompositeStep {
      id: s.id.clone(),
      name: s.name.clone(),
      run: s.run.clone(),
      shell: s.shell.clone(),
      env: s.env.clone(),
      condition: s.condition.clone(),
      uses: s.uses.clone(),
      continue_on_error: s.continue_on_error,
      with: s.with.clone(),
    })
    .collect()
}
