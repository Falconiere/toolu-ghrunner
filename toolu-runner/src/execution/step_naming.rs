use std::collections::HashMap;
use std::path::PathBuf;

use shared::ActionStep;

// ── Post-step types ─────────────────────────────────────────────────

/// A registered post-step to execute after main steps complete (LIFO).
#[derive(Debug, Clone)]
pub struct PostStep {
  pub step_id: String,
  pub action_name: String,
  pub action_dir: PathBuf,
  pub script: String,
  pub condition: Option<String>,
  pub inputs: HashMap<String, String>,
  pub state: HashMap<String, String>,
  pub using: String,
}

impl PostStep {
  /// Effective condition: defaults to `always()` if none specified.
  pub fn effective_condition(&self) -> &str {
    self.condition.as_deref().unwrap_or("always()")
  }
}

/// Queue for post-steps. Registered during main execution, drained LIFO.
#[derive(Debug, Default)]
pub struct PostStepQueue {
  steps: Vec<PostStep>,
}

impl PostStepQueue {
  pub fn new() -> Self {
    Self::default()
  }

  /// Register a post-step for later execution.
  pub fn register(&mut self, step: PostStep) {
    self.steps.push(step);
  }

  /// Drain all post-steps in LIFO order (last registered runs first).
  pub fn drain_lifo(&mut self) -> Vec<PostStep> {
    let mut result = std::mem::take(&mut self.steps);
    result.reverse();
    result
  }
}

/// Produce a display name for a step, matching actions/runner conventions.
pub(super) fn derive_step_name(step: &ActionStep) -> String {
  if let Some(name) = step
    .display_name_token
    .as_ref()
    .and_then(|t| t.to_string_value())
  {
    return name.to_owned();
  }

  if step.is_run_step() {
    let script = step.script_body().unwrap_or_default();
    let first_line = script.lines().next().unwrap_or("").trim();
    if !first_line.is_empty() {
      let truncated: String = first_line.chars().take(60).collect();
      return format!("Run {truncated}");
    }
    return "Run".to_owned();
  }

  if let Some(action) = step
    .reference
    .name
    .as_deref()
    .or(step.reference.image.as_deref())
  {
    if let Some(ref_tag) = step.reference.git_ref.as_deref() {
      return format!("Run {action}@{ref_tag}");
    }
    return format!("Run {action}");
  }

  step.id.clone()
}
