//! Runner plugin trait definition.

use async_trait::async_trait;
use shared::{ActionStep, Conclusion, RunnerEvent};
use tokio::sync::mpsc;

use crate::execution::context::ExecutionContext;

/// A compiled-in plugin that can handle custom step types and hook into
/// the job lifecycle.
///
/// During handler dispatch, registered plugins are checked first --
/// if a plugin's `name()` matches the step's `runs.using` value, the
/// plugin handles that step.
#[async_trait]
pub trait RunnerPlugin: Send + Sync {
  /// The identifier for this plugin. Matched against `runs.using` in action manifests.
  fn name(&self) -> &str;

  /// Called before the first step executes. Can modify the execution context.
  async fn on_job_init(&self, _ctx: &mut ExecutionContext) {}

  /// Execute a step that matched this plugin's name.
  async fn execute_step(
    &self,
    step: &ActionStep,
    ctx: &ExecutionContext,
    events: &mpsc::Sender<RunnerEvent>,
  ) -> Conclusion;

  /// Called after the last step completes (including post steps). Always runs.
  async fn on_job_cleanup(&self, _ctx: &ExecutionContext) {}
}
