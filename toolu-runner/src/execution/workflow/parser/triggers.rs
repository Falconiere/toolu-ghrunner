//! Workflow trigger/event parsing from the `on:` key.

use crate::execution::workflow::types::{BranchFilter, TriggerConfig};

pub(super) fn parse_trigger(on: &Option<serde_yaml::Value>) -> TriggerConfig {
  let Some(value) = on else {
    return TriggerConfig::default();
  };

  let mut config = TriggerConfig::default();

  match value {
    serde_yaml::Value::String(event) => {
      config.event_names.push(event.clone());
    },
    serde_yaml::Value::Sequence(events) => {
      for e in events {
        if let serde_yaml::Value::String(name) = e {
          config.event_names.push(name.clone());
        }
      }
    },
    serde_yaml::Value::Mapping(map) => {
      for (key, val) in map {
        if let serde_yaml::Value::String(event_name) = key {
          config.event_names.push(event_name.clone());
          match event_name.as_str() {
            "push" => config.push = Some(parse_branch_filter(val)),
            "pull_request" => config.pull_request = Some(parse_branch_filter(val)),
            _ => {},
          }
        }
      }
    },
    // `on: true` (YAML boolean) or other non-standard -- treat as empty
    serde_yaml::Value::Null
    | serde_yaml::Value::Bool(_)
    | serde_yaml::Value::Number(_)
    | serde_yaml::Value::Tagged(_) => {},
  }

  config
}

fn parse_branch_filter(value: &serde_yaml::Value) -> BranchFilter {
  let mut filter = BranchFilter::default();
  if let serde_yaml::Value::Mapping(map) = value {
    if let Some(branches) = map.get(serde_yaml::Value::String("branches".to_owned())) {
      filter.branches = yaml_string_list(branches);
    }
    if let Some(tags) = map.get(serde_yaml::Value::String("tags".to_owned())) {
      filter.tags = yaml_string_list(tags);
    }
    if let Some(paths) = map.get(serde_yaml::Value::String("paths".to_owned())) {
      filter.paths = yaml_string_list(paths);
    }
  }
  filter
}

fn yaml_string_list(value: &serde_yaml::Value) -> Vec<String> {
  let serde_yaml::Value::Sequence(seq) = value else {
    return vec![];
  };
  seq
    .iter()
    .filter_map(|v| {
      if let serde_yaml::Value::String(s) = v {
        Some(s.clone())
      } else {
        None
      }
    })
    .collect()
}
