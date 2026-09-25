//! Scoped expression outputs and private action state.

use std::collections::HashMap;

use shared::Conclusion;

use super::{ExecutionContext, StepState};

/// Per-step output / state / conclusion recording.
impl ExecutionContext {
  /// Enter one composite invocation's isolated expression scope.
  pub(in crate::execution) fn enter_step_scope(
    &mut self,
    wire_id: &str,
    inputs: &HashMap<String, String>,
    action_dir: &std::path::Path,
  ) {
    self.scope_path.push(wire_id.to_owned());
    self
      .scoped_inputs
      .insert(self.scope_path.clone(), inputs.clone());
    self.scoped_status.insert(
      self.scope_path.clone(),
      expressions::evaluator::JobStatus::Success,
    );
    self.scoped_action_paths.insert(
      self.scope_path.clone(),
      action_dir.to_string_lossy().into_owned(),
    );
  }

  /// Update the active composite's condition status, or the job when unscoped.
  pub(in crate::execution) fn set_scope_status(
    &mut self,
    status: expressions::evaluator::JobStatus,
  ) {
    if self.scope_path.is_empty() {
      self.job_status = status;
    } else {
      self.scoped_status.insert(self.scope_path.clone(), status);
    }
  }

  /// Snapshot the current scope for a post action registered inside a composite.
  pub(in crate::execution) fn scope_path(&self) -> Vec<String> {
    self.scope_path.clone()
  }

  /// Restore the parent scope after a composite or post action finishes.
  pub(in crate::execution) fn restore_step_scope(&mut self, path: Vec<String>) {
    self.scope_path = path;
  }

  fn current_steps_mut(&mut self) -> &mut HashMap<String, StepState> {
    if self.scope_path.is_empty() {
      &mut self.steps
    } else {
      self
        .scoped_steps
        .entry(self.scope_path.clone())
        .or_default()
    }
  }

  fn current_steps(&self) -> Option<&HashMap<String, StepState>> {
    if self.scope_path.is_empty() {
      Some(&self.steps)
    } else {
      self.scoped_steps.get(&self.scope_path)
    }
  }

  /// Record an output under a visible `steps.<context_name>` entry.
  pub fn set_step_output(&mut self, context_name: &str, key: &str, value: &str) {
    let state = self
      .current_steps_mut()
      .entry(context_name.to_owned())
      .or_default();
    state.outputs.insert(key.to_owned(), value.to_owned());
  }

  /// Record a `save-state` value for a step, surfaced to its post step.
  pub fn set_step_state(&mut self, step_id: &str, key: &str, value: &str) {
    self
      .action_states
      .entry((self.scope_path.clone(), step_id.to_owned()))
      .or_default()
      .insert(key.to_owned(), value.to_owned());
  }

  /// Read the recorded outputs for a visible expression name (empty if none).
  pub fn step_outputs(&self, context_name: &str) -> HashMap<String, String> {
    self
      .current_steps()
      .and_then(|steps| steps.get(context_name))
      .map(|s| s.outputs.clone())
      .unwrap_or_default()
  }

  /// Read the recorded `save-state` map for a step, surfaced as `STATE_*`
  /// to that same step's pre/main/post stages (empty if none).
  pub fn step_state(&self, step_id: &str) -> HashMap<String, String> {
    self
      .action_states
      .get(&(self.scope_path.clone(), step_id.to_owned()))
      .cloned()
      .unwrap_or_default()
  }

  /// Record a step's REAL result (`steps.<context_name>.outcome`), before any
  /// `continue-on-error` adjustment.
  pub fn set_step_outcome(&mut self, context_name: &str, outcome: Conclusion) {
    let state = self
      .current_steps_mut()
      .entry(context_name.to_owned())
      .or_default();
    state.outcome = Some(outcome);
  }

  /// Record a step's effective result (`steps.<id>.conclusion`), after
  /// `continue-on-error` (equals the outcome unless the step failed with
  /// `continue-on-error: true`).
  pub fn set_step_conclusion(&mut self, context_name: &str, conclusion: Conclusion) {
    let state = self
      .current_steps_mut()
      .entry(context_name.to_owned())
      .or_default();
    state.conclusion = Some(conclusion);
  }
}
