use std::collections::{HashMap, HashSet, VecDeque};

use shared::RunnerError;

/// Build a topological order of jobs from dependency map.
///
/// Returns job IDs in execution order. Detects cycles.
///
/// # Errors
///
/// Returns `RunnerError::Expression` if a cycle is detected.
pub fn topological_sort(jobs: &HashMap<String, Vec<String>>) -> Result<Vec<String>, RunnerError> {
  let mut in_deg: HashMap<&str, usize> = HashMap::new();
  let mut dependents: HashMap<&str, Vec<&str>> = HashMap::new();

  for (job, deps) in jobs {
    in_deg.insert(job.as_str(), deps.len());
    for dep in deps {
      in_deg.entry(dep.as_str()).or_insert(0);
      dependents
        .entry(dep.as_str())
        .or_default()
        .push(job.as_str());
    }
  }

  let mut queue: VecDeque<&str> = in_deg
    .iter()
    .filter(|(_, deg)| **deg == 0)
    .map(|(job, _)| *job)
    .collect();

  let mut order = Vec::new();

  while let Some(job) = queue.pop_front() {
    order.push(job.to_owned());

    if let Some(deps) = dependents.get(job) {
      for dependent in deps {
        if let Some(deg) = in_deg.get_mut(dependent) {
          *deg = deg.saturating_sub(1);
          if *deg == 0 {
            queue.push_back(dependent);
          }
        }
      }
    }
  }

  if order.len() != jobs.len() {
    return Err(RunnerError::Expression(
      "cycle detected in job dependencies".to_owned(),
    ));
  }

  Ok(order)
}

/// Get jobs that are ready to execute (all dependencies completed).
pub fn ready_jobs(
  jobs: &HashMap<String, Vec<String>>,
  completed: &HashSet<String>,
  running: &HashSet<String>,
) -> Vec<String> {
  jobs
    .iter()
    .filter(|(id, deps)| {
      !completed.contains(*id)
        && !running.contains(*id)
        && deps.iter().all(|dep| completed.contains(dep))
    })
    .map(|(id, _)| id.clone())
    .collect()
}
