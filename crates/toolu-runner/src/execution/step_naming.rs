use std::path::PathBuf;

use shared::ActionStep;

use super::actions::manifest::ActionDefinition;

// ── Post-step types ─────────────────────────────────────────────────

/// A registered post-step to execute after main steps complete (LIFO).
///
/// Carries everything needed to re-run the action's `post` node entrypoint in
/// the same step scope at job end. `STATE_*` is *not* snapshotted here — it is
/// read fresh from the live context at drain time so `post` sees whatever
/// `main` saved.
#[derive(Debug, Clone)]
pub struct PostStep {
  /// The originating action step (same id/scope as `main`).
  pub step: ActionStep,
  /// Human-readable action name (for the `Post <name>` step header).
  pub action_name: String,
  /// Resolved on-disk action directory (the cached action root).
  pub action_dir: PathBuf,
  /// Parsed action manifest (carries the `post` entrypoint + inputs).
  pub manifest: ActionDefinition,
  /// Node major version (`runs.using: node20` → 20).
  pub major: u8,
  /// Explicit `post-if`, if any; otherwise the effective default applies.
  pub condition: Option<String>,
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
  /// Create an empty post-step queue.
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
