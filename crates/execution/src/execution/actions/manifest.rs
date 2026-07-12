use std::collections::HashMap;

use serde::Deserialize;
use shared::RunnerError;

/// Parsed action definition from action.yml/action.yaml.
#[derive(Debug, Clone)]
pub struct ActionDefinition {
  pub name: String,
  pub description: String,
  pub inputs: HashMap<String, ActionInput>,
  pub outputs: HashMap<String, ActionOutput>,
  pub runs: ActionRuns,
}

/// An input parameter for an action.
#[derive(Debug, Clone)]
pub struct ActionInput {
  pub description: String,
  pub required: bool,
  pub default: Option<String>,
}

/// An output parameter for an action.
#[derive(Debug, Clone)]
pub struct ActionOutput {
  pub description: String,
  pub value: Option<String>,
}

/// A single step within a composite action's `steps:` array.
#[derive(Debug, Clone)]
pub struct CompositeStep {
  pub id: Option<String>,
  pub name: Option<String>,
  pub run: Option<String>,
  pub shell: Option<String>,
  pub env: HashMap<String, String>,
  pub condition: Option<String>,
  pub uses: Option<String>,
  pub continue_on_error: bool,
  pub with: HashMap<String, String>,
}

/// The `runs` section of an action manifest.
#[derive(Debug, Clone)]
pub struct ActionRuns {
  pub using: RunsUsing,
  pub main: Option<String>,
  pub pre: Option<String>,
  pub post: Option<String>,
  pub pre_if: Option<String>,
  pub post_if: Option<String>,
  pub image: Option<String>,
  pub steps: Vec<CompositeStep>,
}

/// The runtime type from `runs.using`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunsUsing {
  /// Node.js action — major version from `runs.using` (e.g. 20, 24).
  Node {
    major: u8,
  },
  Composite,
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

  let inputs = raw
    .inputs
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
    .collect();

  let outputs = raw
    .outputs
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
    .collect();

  let steps = parse_composite_steps(&runs_raw.steps);

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

fn parse_composite_steps(raw: &Option<Vec<RawCompositeStep>>) -> Vec<CompositeStep> {
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
