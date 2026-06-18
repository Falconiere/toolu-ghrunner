use std::collections::HashMap;

use crate::execution::actions::manifest::ActionDefinition;

/// Determine which script to run for a given stage (pre/main/post).
///
/// Returns `None` if the action doesn't define a script for that stage.
pub fn determine_script(def: &ActionDefinition, stage: &str) -> Option<String> {
  match stage {
    "pre" => def.runs.pre.clone(),
    "main" => def.runs.main.clone(),
    "post" => def.runs.post.clone(),
    _ => None,
  }
}

/// Convert an input name to its environment variable key.
///
/// GitHub Actions convention: `INPUT_{NAME}` where name is uppercased.
/// Hyphens are preserved (NOT replaced with underscores).
pub fn input_env_key(name: &str) -> String {
  format!("INPUT_{}", name.to_uppercase())
}

/// Build the environment variables for a Node.js action step.
///
/// Includes:
/// - `GITHUB_ACTION_PATH` — path to the action directory
/// - `INPUT_{NAME}` — for each input (from step inputs or action defaults)
/// - `STATE_{KEY}` — state values from previous steps
///
/// Does NOT set `NODE_OPTIONS` (blocked).
pub fn build_action_env(
  def: &ActionDefinition,
  step_inputs: &HashMap<String, String>,
  action_path: &str,
  state: &HashMap<String, String>,
) -> HashMap<String, String> {
  let mut env = HashMap::new();

  env.insert("GITHUB_ACTION_PATH".to_owned(), action_path.to_owned());

  // Set input env vars: step inputs override action defaults
  for (name, input_def) in &def.inputs {
    let value = step_inputs
      .get(name)
      .cloned()
      .or_else(|| input_def.default.clone());

    if let Some(val) = value {
      env.insert(input_env_key(name), val);
    }
  }

  // Set STATE_* env vars
  for (key, value) in state {
    env.insert(format!("STATE_{key}"), value.clone());
  }

  env
}
