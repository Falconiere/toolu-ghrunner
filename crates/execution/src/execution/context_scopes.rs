//! Temporary environment overlays and deferred nested Node cleanup posts.

use std::collections::HashMap;

use super::ExecutionContext;
use super::job_status_str;
use crate::execution::step_naming::PostStep;
use expressions::types::ExprValue;

impl ExecutionContext {
  /// Overlay the current composite action path and status on github context.
  pub(super) fn scoped_github_context(&self) -> ExprValue {
    let mut github = self.github.clone();
    if let Some(path) = self.scoped_action_paths.get(&self.scope_path) {
      github.insert("action_path".to_owned(), ExprValue::String(path.clone()));
    }
    if let Some(status) = self.scoped_status.get(&self.scope_path) {
      github.insert(
        "action_status".to_owned(),
        ExprValue::String(job_status_str(*status).to_owned()),
      );
    }
    ExprValue::Object(github)
  }

  /// Return persistent job env with active nested step overlays applied.
  pub(super) fn visible_env(&self) -> HashMap<String, String> {
    let mut env = self.env.clone();
    for overlay in &self.step_env_overlays {
      env.extend(overlay.clone());
    }
    env
  }

  /// Capture only active step scopes for a deferred post; later job env stays live.
  pub(in crate::execution) fn snapshot_step_env(&self) -> HashMap<String, String> {
    let mut env = HashMap::new();
    for overlay in &self.step_env_overlays {
      env.extend(overlay.clone());
    }
    env
  }

  /// Make one nested `uses:` step's env visible only for its action stages.
  pub(in crate::execution) fn push_step_env(&mut self, env: HashMap<String, String>) {
    self.step_env_overlays.push(env);
  }

  /// Restore the parent environment after a nested action returns.
  pub(in crate::execution) fn pop_step_env(&mut self) {
    if self.step_env_overlays.pop().is_none() {
      tracing::warn!("nested step environment stack was empty during restoration");
    }
  }

  /// Register a nested Node post for the job's LIFO cleanup queue.
  pub(in crate::execution) fn register_nested_post(&mut self, post: PostStep) {
    self.nested_posts.push(post);
  }

  /// Transfer all nested posts registered by the current top-level step.
  pub(in crate::execution) fn take_nested_posts(&mut self) -> Vec<PostStep> {
    std::mem::take(&mut self.nested_posts)
  }
}
