use std::collections::HashMap;

use super::types::TriggerConfig;

/// Event payload for trigger evaluation.
#[derive(Debug, Clone)]
pub struct EventPayload {
  /// The triggering event's name (`push`, `pull_request`, `workflow_dispatch`, ...).
  pub event_name: String,
  /// Branch the event occurred on, if applicable.
  pub branch: Option<String>,
  /// Tag the event occurred on, if applicable.
  pub tag: Option<String>,
  /// Paths touched by the event, for `paths:`/`paths-ignore:` filters.
  pub paths_changed: Vec<String>,
  /// `workflow_dispatch` inputs, if any.
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
