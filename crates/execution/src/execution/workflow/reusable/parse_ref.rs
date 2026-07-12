//! Reusable workflow reference parsing.

use shared::RunnerError;

use super::types::ReusableWorkflowRef;

/// Parse a reusable workflow reference string.
///
/// Expected format: `owner/repo/path/to/workflow.yml@ref`
///
/// # Errors
///
/// Returns `RunnerError::ReusableWorkflow` if the format is invalid.
pub fn parse_reusable_ref(ref_str: &str) -> Result<ReusableWorkflowRef, RunnerError> {
  let at_pos = ref_str.rfind('@').ok_or_else(|| {
    RunnerError::ReusableWorkflow(format!(
      "invalid reusable workflow reference (missing @ref): {ref_str}"
    ))
  })?;

  let path_part = ref_str
    .get(..at_pos)
    .ok_or_else(|| RunnerError::ReusableWorkflow(format!("invalid reference: {ref_str}")))?;
  let git_ref = ref_str
    .get(at_pos + 1..)
    .ok_or_else(|| RunnerError::ReusableWorkflow(format!("invalid reference: {ref_str}")))?;

  if git_ref.is_empty() {
    return Err(RunnerError::ReusableWorkflow(format!(
      "empty ref in reusable workflow reference: {ref_str}"
    )));
  }

  let mut parts = path_part.splitn(3, '/');
  let owner = parts
    .next()
    .ok_or_else(|| RunnerError::ReusableWorkflow(format!("missing owner in: {ref_str}")))?;
  let repo = parts
    .next()
    .ok_or_else(|| RunnerError::ReusableWorkflow(format!("missing repo in: {ref_str}")))?;
  let path = parts.next().ok_or_else(|| {
    RunnerError::ReusableWorkflow(format!(
      "missing workflow path in: {ref_str} (expected owner/repo/path@ref)"
    ))
  })?;

  if path.is_empty() {
    return Err(RunnerError::ReusableWorkflow(format!(
      "empty workflow path in: {ref_str}"
    )));
  }

  Ok(ReusableWorkflowRef {
    owner: owner.to_owned(),
    repo: repo.to_owned(),
    path: path.to_owned(),
    git_ref: git_ref.to_owned(),
  })
}
