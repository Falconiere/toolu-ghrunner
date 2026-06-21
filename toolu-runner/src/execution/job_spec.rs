//! Job-level execution inputs that live above the per-step loop: the job's
//! `outputs:` expression map and the merged `defaults.run` (shell +
//! working-directory) fallback.
//!
//! The wire `AgentJobRequestMessage` does not yet carry these as typed fields
//! (Open Q1 — confirm the wire shape from a live capture), so the live path
//! constructs an empty `JobSpec` today; hermetic tests build it from the real
//! `workflow::types` structures. Once a job message is captured, populate
//! `JobSpec` from it in `job_runner::run_job`.

use std::collections::HashMap;

use shared::RunnerError;

use super::context::ExecutionContext;
use super::workflow::types::{RunDefaults, WorkflowDefaults};

/// Job-level fallback shell + working-directory for `run:` steps.
///
/// Holds the result of merging the workflow-level and job-level `defaults.run`,
/// with the job value taking precedence over the workflow value. Steps that set
/// their own `shell:`/`working-directory:` still win over both (applied at the
/// step site).
#[derive(Debug, Clone, Default)]
pub struct RunDefaultsResolved {
  /// Fallback shell name (e.g. `bash`) when a run-step omits `shell:`.
  pub shell: Option<String>,
  /// Fallback working directory when a run-step omits `working-directory:`.
  pub working_directory: Option<String>,
}

/// Constant-per-job inputs threaded into the step loop and consumed at job end.
#[derive(Debug, Clone, Default)]
pub struct JobSpec {
  /// Job-level `outputs:` map: name → `${{ }}` expression string. Evaluated
  /// against the final context after all steps + post-steps complete.
  pub outputs: HashMap<String, String>,
  /// Merged `defaults.run` fallback applied to run-steps that omit their own.
  pub defaults: RunDefaultsResolved,
}

impl JobSpec {
  /// Build a `JobSpec` from a parsed job's outputs map plus the workflow-level
  /// and job-level `defaults.run`. Job defaults override workflow defaults; a
  /// `None` at the job level falls back to the workflow value.
  pub fn from_workflow(
    outputs: HashMap<String, String>,
    workflow_defaults: Option<&WorkflowDefaults>,
    job_defaults: Option<&WorkflowDefaults>,
  ) -> Self {
    Self {
      outputs,
      defaults: merge_run_defaults(workflow_defaults, job_defaults),
    }
  }
}

/// Merge workflow + job `defaults.run` with job precedence (job > workflow).
fn merge_run_defaults(
  workflow: Option<&WorkflowDefaults>,
  job: Option<&WorkflowDefaults>,
) -> RunDefaultsResolved {
  let wf = workflow.and_then(|d| d.run.as_ref());
  let jb = job.and_then(|d| d.run.as_ref());
  RunDefaultsResolved {
    shell: pick(jb.and_then(|r| r.shell.clone()), wf.and_then(run_shell)),
    working_directory: pick(
      jb.and_then(|r| r.working_directory.clone()),
      wf.and_then(run_wd),
    ),
  }
}

fn run_shell(r: &RunDefaults) -> Option<String> {
  r.shell.clone()
}

fn run_wd(r: &RunDefaults) -> Option<String> {
  r.working_directory.clone()
}

/// Job value wins; fall back to the workflow value when the job omits it.
fn pick(job: Option<String>, workflow: Option<String>) -> Option<String> {
  job.or(workflow)
}

/// Evaluate the job's `outputs:` map against the final execution context.
///
/// Each value is a `${{ }}` expression (typically `steps.<id>.outputs.<k>`).
/// Interpolation runs after all main + post steps complete, so step outputs are
/// fully recorded. The resolved map is placed into `JobCompleted.outputs`.
///
/// # Errors
///
/// Returns `RunnerError::Expression` if an output expression fails to evaluate.
pub fn evaluate_job_outputs(
  spec: &JobSpec,
  ctx: &ExecutionContext,
) -> Result<HashMap<String, String>, RunnerError> {
  let mut resolved = HashMap::with_capacity(spec.outputs.len());
  for (name, expr) in &spec.outputs {
    let value = ctx.interpolate_string(expr)?;
    resolved.insert(name.clone(), value);
  }
  Ok(resolved)
}
