//! Job and step definition parsing from workflow YAML.

use std::collections::HashMap;

use crate::execution::workflow::types::{
  JobDefinition, MatrixConfig, StepDefinition, StrategyConfig,
};

use super::raw_types::{RawJob, RawStep};

pub(super) fn parse_jobs(raw: HashMap<String, RawJob>) -> HashMap<String, JobDefinition> {
  raw
    .into_iter()
    .map(|(name, raw_job)| (name, parse_single_job(raw_job)))
    .collect()
}

/// Parse a YAML value that can be either a single string or a sequence of strings.
/// Returns `default` when the value is `None` or an unsupported type.
fn yaml_string_or_vec(value: Option<serde_yaml::Value>, default: Vec<String>) -> Vec<String> {
  match value {
    Some(serde_yaml::Value::String(s)) => vec![s],
    Some(serde_yaml::Value::Sequence(seq)) => seq
      .into_iter()
      .filter_map(|v| {
        if let serde_yaml::Value::String(s) = v {
          Some(s)
        } else {
          None
        }
      })
      .collect(),
    None | Some(_) => default,
  }
}

fn parse_single_job(raw: RawJob) -> JobDefinition {
  let runs_on = yaml_string_or_vec(raw.runs_on, vec!["ubuntu-latest".to_owned()]);

  let needs = yaml_string_or_vec(raw.needs, vec![]);

  let strategy = raw.strategy.map(|s| StrategyConfig {
    matrix: parse_matrix(s.matrix.unwrap_or_default()),
    fail_fast: s.fail_fast.unwrap_or(true),
    max_parallel: s.max_parallel,
  });

  let steps = raw
    .steps
    .unwrap_or_default()
    .into_iter()
    .map(parse_step)
    .collect();

  JobDefinition {
    runs_on,
    needs,
    if_condition: raw.if_condition,
    env: raw.env.unwrap_or_default(),
    defaults: None,
    permissions: raw.permissions,
    strategy,
    steps,
    outputs: raw.outputs.unwrap_or_default(),
    container: raw.container,
    services: raw.services,
  }
}

fn parse_matrix(raw: serde_yaml::Value) -> MatrixConfig {
  let mut config = MatrixConfig::default();
  if let serde_yaml::Value::Mapping(map) = raw {
    for (key, val) in map {
      let serde_yaml::Value::String(key_str) = key else {
        continue;
      };
      match key_str.as_str() {
        "include" => {
          if let serde_yaml::Value::Sequence(items) = val {
            config.include = items.into_iter().filter_map(yaml_to_string_map).collect();
          }
        },
        "exclude" => {
          if let serde_yaml::Value::Sequence(items) = val {
            config.exclude = items.into_iter().filter_map(yaml_to_string_map).collect();
          }
        },
        _ => {
          if let serde_yaml::Value::Sequence(values) = val {
            config.base.insert(key_str, values);
          }
        },
      }
    }
  }
  config
}

fn yaml_to_string_map(value: serde_yaml::Value) -> Option<HashMap<String, serde_yaml::Value>> {
  if let serde_yaml::Value::Mapping(map) = value {
    let mut result = HashMap::new();
    for (k, v) in map {
      if let serde_yaml::Value::String(key) = k {
        result.insert(key, v);
      }
    }
    Some(result)
  } else {
    None
  }
}

fn parse_step(raw: RawStep) -> StepDefinition {
  StepDefinition {
    id: raw.id,
    name: raw.name,
    uses: raw.uses,
    run: raw.run,
    shell: raw.shell,
    with: raw.with.unwrap_or_default(),
    env: raw.env.unwrap_or_default(),
    if_condition: raw.if_condition,
    continue_on_error: raw.continue_on_error.unwrap_or(false),
    timeout_minutes: raw.timeout_minutes,
    working_directory: raw.working_directory,
  }
}
