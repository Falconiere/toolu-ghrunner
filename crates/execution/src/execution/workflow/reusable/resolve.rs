//! Depth limits, circular reference checks, and invocation resolution.

use std::collections::HashMap;

use shared::RunnerError;

use super::types::{ReusableWorkflowDef, SecretMode};

/// Maximum nesting depth for reusable workflows.
/// Matches GitHub's limit: caller -> level1 -> level2 -> level3 -> level4.
pub const MAX_REUSABLE_WORKFLOW_DEPTH: u32 = 4;

/// Check that the current nesting depth does not exceed the maximum.
///
/// # Errors
///
/// Returns `RunnerError::ReusableWorkflow` if depth exceeds the limit.
pub fn check_nesting_depth(depth: u32) -> Result<(), RunnerError> {
  if depth > MAX_REUSABLE_WORKFLOW_DEPTH {
    return Err(RunnerError::ReusableWorkflow(format!(
      "maximum reusable workflow depth of {MAX_REUSABLE_WORKFLOW_DEPTH} exceeded (current depth: {depth})"
    )));
  }
  Ok(())
}

/// Check for circular references in the reusable workflow call stack.
///
/// # Errors
///
/// Returns `RunnerError::ReusableWorkflow` if a circular reference is found.
pub fn check_circular_reference(call_stack: &[String], new_ref: &str) -> Result<(), RunnerError> {
  if call_stack.iter().any(|r| r == new_ref) {
    return Err(RunnerError::ReusableWorkflow(format!(
      "circular reusable workflow reference detected: '{new_ref}' already in call stack: [{}]",
      call_stack.join(" -> ")
    )));
  }
  Ok(())
}

/// Context for resolving a reusable workflow invocation.
#[derive(Debug, Clone)]
pub struct ResolveContext {
  pub call_stack: Vec<String>,
  pub current_ref: String,
  pub current_depth: u32,
}

/// Result of resolving a reusable workflow invocation.
#[derive(Debug, Clone)]
pub struct ResolvedInvocation {
  pub inputs: HashMap<String, String>,
  pub secrets: HashMap<String, String>,
}

/// Validate and resolve a reusable workflow invocation.
///
/// # Errors
///
/// Returns `RunnerError::ReusableWorkflow` on depth, circular ref, or validation failures.
pub fn resolve_reusable_invocation(
  workflow_def: &ReusableWorkflowDef,
  caller_inputs: &HashMap<String, String>,
  secret_mode: &SecretMode,
  caller_secrets: &HashMap<String, String>,
  resolve_ctx: &ResolveContext,
) -> Result<ResolvedInvocation, RunnerError> {
  check_nesting_depth(resolve_ctx.current_depth)?;
  check_circular_reference(&resolve_ctx.call_stack, &resolve_ctx.current_ref)?;
  super::types::validate_inputs(&workflow_def.inputs, caller_inputs)?;
  let inputs = super::types::resolve_inputs(&workflow_def.inputs, caller_inputs);
  let secrets = super::types::validate_secrets(secret_mode, &workflow_def.secrets, caller_secrets)?;
  Ok(ResolvedInvocation { inputs, secrets })
}
