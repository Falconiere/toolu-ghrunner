//! Top-level workflow YAML parsing entry point.

use super::raw_types::RawWorkflow;
use crate::execution::workflow::types::WorkflowDefinition;
use shared::RunnerError;

/// Parse a workflow YAML string into a `WorkflowDefinition`.
///
/// # Errors
///
/// Returns `RunnerError::Expression` on YAML parse failures.
pub fn parse_workflow(yaml: &str) -> Result<WorkflowDefinition, RunnerError> {
  let raw: RawWorkflow = serde_yaml::from_str(yaml)
    .map_err(|e| RunnerError::Expression(format!("workflow YAML parse: {e}")))?;

  let on = super::triggers::parse_trigger(&raw.on);
  let jobs = super::jobs::parse_jobs(raw.jobs.unwrap_or_default());

  Ok(WorkflowDefinition {
    name: raw.name,
    on,
    env: raw.env.unwrap_or_default(),
    defaults: None,
    permissions: raw.permissions,
    jobs,
  })
}
