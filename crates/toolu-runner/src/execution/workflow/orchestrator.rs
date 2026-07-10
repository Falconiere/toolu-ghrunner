use std::collections::{HashMap, HashSet};

use shared::{Conclusion, RunnerError};

use super::job_graph::{ready_jobs, topological_sort};
use super::matrix::expand_matrix;
use super::parser::parse_workflow;
use super::trigger::{EventPayload, evaluate_triggers};
use super::types::WorkflowDefinition;

/// Conclusion for an entire workflow run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowConclusion {
  Success,
  Failure,
  Cancelled,
}

/// Result of a single job execution.
#[derive(Debug, Clone)]
pub struct JobResult {
  pub job_id: String,
  pub conclusion: Conclusion,
  pub matrix: Option<HashMap<String, String>>,
}

/// Result of workflow orchestration.
#[derive(Debug)]
pub struct WorkflowResult {
  pub conclusion: WorkflowConclusion,
  pub job_results: Vec<JobResult>,
}

/// Run a workflow from YAML.
///
/// Parses YAML, evaluates triggers, builds DAG, expands matrix,
/// and executes jobs in dependency order (simulated in this slice).
///
/// # Errors
///
/// Returns `RunnerError` on parse, cycle, or trigger failures.
pub fn run_workflow(yaml: &str, event: &EventPayload) -> Result<WorkflowResult, RunnerError> {
  let wf = parse_workflow(yaml)?;

  let trigger_matches = evaluate_triggers(&wf.on, event);

  if !trigger_matches {
    return Ok(WorkflowResult {
      conclusion: WorkflowConclusion::Success,
      job_results: vec![],
    });
  }

  // Build dependency graph and validate (detects cycles)
  let deps = build_dep_map(&wf);
  let _order = topological_sort(&deps)?;

  Ok(execute_workflow(&wf, &deps))
}

fn build_dep_map(wf: &WorkflowDefinition) -> HashMap<String, Vec<String>> {
  wf.jobs
    .iter()
    .map(|(id, job)| (id.clone(), job.needs.clone()))
    .collect()
}

fn execute_workflow(
  wf: &WorkflowDefinition,
  deps: &HashMap<String, Vec<String>>,
) -> WorkflowResult {
  let mut completed: HashSet<String> = HashSet::new();
  let mut job_results = Vec::new();
  let mut overall = WorkflowConclusion::Success;

  loop {
    let ready = ready_jobs(deps, &completed, &HashSet::new());
    if ready.is_empty() {
      break;
    }

    for job_id in &ready {
      let job_def = wf.jobs.get(job_id);

      // Expand matrix if present
      let matrix_combos = job_def
        .and_then(|j| j.strategy.as_ref())
        .map(|s| expand_matrix(&s.matrix))
        .unwrap_or_else(|| vec![HashMap::new()]);

      for combo in &matrix_combos {
        // SIMULATED execution — always succeeds in this slice
        let matrix = if combo.is_empty() {
          None
        } else {
          Some(combo.clone())
        };

        job_results.push(JobResult {
          job_id: job_id.clone(),
          conclusion: Conclusion::Success,
          matrix,
        });
      }

      completed.insert(job_id.clone());
    }
  }

  // Check if any jobs weren't reached (shouldn't happen after topo sort)
  if completed.len() != deps.len() {
    overall = WorkflowConclusion::Failure;
  }

  WorkflowResult {
    conclusion: overall,
    job_results,
  }
}
