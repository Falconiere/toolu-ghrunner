use std::collections::HashMap;

use super::types::TriggerConfig;

/// Event payload for trigger evaluation.
#[derive(Debug, Clone)]
pub struct EventPayload {
  pub event_name: String,
  pub branch: Option<String>,
  pub tag: Option<String>,
  pub paths_changed: Vec<String>,
  pub inputs: HashMap<String, String>,
}

/// Evaluate whether workflow triggers match the event payload.
pub fn evaluate_triggers(config: &TriggerConfig, event: &EventPayload) -> bool {
  // Check if event name is in the trigger list
  if !config.event_names.contains(&event.event_name) {
    return false;
  }

  // For push events, check branch/tag filters
  if event.event_name == "push"
    && let Some(push_filter) = &config.push
    && !push_filter.branches.is_empty()
  {
    let branch = event.branch.as_deref().unwrap_or_default();
    if !push_filter.branches.iter().any(|b| matches_glob(b, branch)) {
      return false;
    }
  }

  // For pull_request events, check branch filters
  if event.event_name == "pull_request"
    && let Some(pr_filter) = &config.pull_request
    && !pr_filter.branches.is_empty()
  {
    let branch = event.branch.as_deref().unwrap_or_default();
    if !pr_filter.branches.iter().any(|b| matches_glob(b, branch)) {
      return false;
    }
  }

  true
}

/// Simple glob matching supporting `*` and `**`.
fn matches_glob(pattern: &str, value: &str) -> bool {
  if pattern == "*" || pattern == "**" {
    return true;
  }
  if let Some(prefix) = pattern.strip_suffix("**") {
    return value.starts_with(prefix);
  }
  if let Some(prefix) = pattern.strip_suffix('*') {
    return value.starts_with(prefix);
  }
  pattern == value
}
