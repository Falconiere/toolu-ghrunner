//! Startup adoption: rebuilding every live job from container labels so a
//! daemon restart resumes where the previous process left off instead of
//! believing the box is empty.
//!
//! **This is a routine path, not an edge case.** Rotating the bearer token,
//! shipping a new daemon binary and a systemd restart all land here, and
//! `crates/daemon/README.md` puts the reason plainly: state lives in Docker,
//! not in this process. Without adoption a restart would reset the resource
//! gate to zero and then admit — and start — a whole box's worth of new work
//! on top of every container still running, overcommitting the machine by
//! exactly what it was already doing.
//!
//! Two rules the rest of this crate depends on:
//!
//! - **The deadline comes from the `sh.toolu.deadline` label, never from
//!   `Config.Env`.** That env block also carries `TOOLU_JITCONFIG`, a
//!   single-use GitHub credential nothing outside the container may read, and
//!   the daemon has no reason to open it. `crate::docker::inventory` reads the
//!   label; this module never sees an environment at all.
//! - **Only a container Docker reports *running* consumes budget.** One that
//!   exited is finished work and gets removed, not restarted — restarting it
//!   would boot a runner against a JIT config GitHub already consumed. One
//!   that was created but never started is the narrow window where the
//!   previous process died between `docker create` and `docker start`: it
//!   holds no budget yet, so it is re-queued and `crate::reaper::reconcile`
//!   starts it when budget allows.
//!
//! Everything here is pure — no bollard, no Docker socket, no clock of its
//! own. `now_ms` arrives as an argument exactly as it does in
//! `crate::reaper::reconcile`, so every decision is deterministic and
//! testable without a daemon.

use crate::gate::{Gate, JobId, JobSize};
use crate::reaper::{CreatedContainers, StartQueue};

/// Where in its lifecycle Docker reports one of this daemon's containers —
/// reduced to the three cases adoption actually distinguishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerLifecycle {
  /// `docker create` finished but the container was never started: the
  /// previous process died in the window between the two.
  Created,
  /// Live right now, holding real vCPU and memory on the box.
  Running,
  /// Over — exited, dead or being removed. Its job is done, whatever the
  /// exit code.
  Finished,
}

/// One container found on the box at startup, as
/// `crate::docker::inventory` reads it back: identity and deadline from the
/// `sh.toolu.job-id`/`sh.toolu.deadline` labels, footprint from the limits
/// `docker create` recorded, and where Docker says it is in its lifecycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdoptedContainer {
  /// GitHub's own job id, from the `sh.toolu.job-id` label.
  pub job_id: String,
  /// The container's id.
  pub container_id: String,
  /// Epoch milliseconds the job must finish by, from the
  /// `sh.toolu.deadline` label — never from `Config.Env`.
  pub deadline_ms: i64,
  /// The vCPU/memory the container holds, read back off its Docker limits.
  pub size: JobSize,
  /// Where Docker reports it in its lifecycle.
  pub lifecycle: ContainerLifecycle,
}

impl AdoptedContainer {
  /// Build one container's adoption input.
  pub fn new(
    job_id: impl Into<String>,
    container_id: impl Into<String>,
    deadline_ms: i64,
    size: JobSize,
    lifecycle: ContainerLifecycle,
  ) -> Self {
    Self {
      job_id: job_id.into(),
      container_id: container_id.into(),
      deadline_ms,
      size,
      lifecycle,
    }
  }
}

/// What adoption decided to do with one container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdoptionAction {
  /// Running and inside its deadline: seed its footprint as consumed budget.
  Resume,
  /// Created but never started, and inside its deadline: track it and queue
  /// it to start.
  Requeue,
  /// Past its deadline, or already finished: remove it and charge nothing.
  Remove,
}

/// A job adoption brought back, with the deadline the reaper now enforces
/// for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArmedJob {
  /// GitHub's own job id.
  pub job_id: JobId,
  /// The container serving it.
  pub container_id: String,
  /// Epoch milliseconds it must finish by — from its label, re-armed for
  /// `crate::reaper::reconcile` to enforce on the next tick.
  pub deadline_ms: i64,
}

/// What one [`adopt`] pass rebuilt, in terms its caller acts on.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Adoption {
  /// Jobs resumed as running, holding budget again.
  pub resumed: Vec<ArmedJob>,
  /// Jobs whose containers exist but never started, now queued to start.
  pub requeued: Vec<ArmedJob>,
  /// Container ids to force-remove before serving: finished, past their
  /// deadline, or impossible to run under the current budget.
  pub remove: Vec<String>,
}

/// What to do with `container`, given the wall clock at startup.
///
/// The deadline is checked first and independently of lifecycle: a container
/// past its deadline is moot to GitHub whether or not it is still running, so
/// it never gets to consume budget on the way out.
pub fn action_for(container: &AdoptedContainer, now_ms: i64) -> AdoptionAction {
  if container.deadline_ms <= now_ms {
    return AdoptionAction::Remove;
  }
  match container.lifecycle {
    ContainerLifecycle::Running => AdoptionAction::Resume,
    ContainerLifecycle::Created => AdoptionAction::Requeue,
    ContainerLifecycle::Finished => AdoptionAction::Remove,
  }
}

/// Rebuild the daemon's whole in-memory picture from `containers` — the job
/// containers Docker still has — seeding `gate` with the budget the running
/// ones consume, `queue` with the ones still waiting to start, and `created`
/// with every container id so a redelivered `POST /v1/jobs` stays idempotent
/// and `crate::reaper::reconcile` knows which container to start for a
/// promoted job.
///
/// Call this once, at startup, before the listener binds: a request served
/// against an empty gate would be admitted against budget the box does not
/// have.
pub fn adopt(
  gate: &mut Gate,
  queue: &mut StartQueue,
  created: &mut CreatedContainers,
  containers: &[AdoptedContainer],
  now_ms: i64,
) -> Adoption {
  let mut adoption = Adoption::default();
  for container in containers {
    match action_for(container, now_ms) {
      AdoptionAction::Resume => resume(gate, created, container, &mut adoption),
      AdoptionAction::Requeue => requeue(gate, queue, created, container, &mut adoption),
      AdoptionAction::Remove => adoption.remove.push(container.container_id.clone()),
    }
  }
  adoption
}

/// Seed a running container's footprint as consumed budget.
///
/// A job id the gate already holds means two containers claim to be the same
/// job — a create redelivered across the restart, most likely. The gate can
/// only account for one of them, so the second is removed rather than left
/// running untracked, which would overcommit the box by exactly its size.
fn resume(
  gate: &mut Gate,
  created: &mut CreatedContainers,
  container: &AdoptedContainer,
  adoption: &mut Adoption,
) {
  let job_id = JobId::new(container.job_id.clone());
  match gate.adopt_running(&job_id, container.size) {
    Ok(()) => {
      created.record(&container.job_id, &container.container_id);
      adoption.resumed.push(armed(&job_id, container));
    },
    Err(err) => {
      tracing::warn!(
        job_id = container.job_id.as_str(),
        container_id = container.container_id.as_str(),
        error = %err,
        "two containers claim the same job id; removing the duplicate"
      );
      adoption.remove.push(container.container_id.clone());
    },
  }
}

/// Track a created-but-never-started container and queue it to start.
///
/// Admission can still refuse it — the box's budget may have been narrowed
/// below this job's size since it was created, or the queue ceiling lowered —
/// and a job that can never start is not one to keep waiting for: its
/// container is removed, and the deadline the runner's own watchdog carries
/// would have expired it anyway.
fn requeue(
  gate: &mut Gate,
  queue: &mut StartQueue,
  created: &mut CreatedContainers,
  container: &AdoptedContainer,
  adoption: &mut Adoption,
) {
  let job_id = JobId::new(container.job_id.clone());
  match gate.admit(&job_id, container.size) {
    Ok(()) => {
      created.record(&container.job_id, &container.container_id);
      queue.push(job_id.clone());
      adoption.requeued.push(armed(&job_id, container));
    },
    Err(err) => {
      tracing::warn!(
        job_id = container.job_id.as_str(),
        container_id = container.container_id.as_str(),
        error = %err,
        "an unstarted container cannot be admitted under this budget; removing it"
      );
      adoption.remove.push(container.container_id.clone());
    },
  }
}

/// The [`ArmedJob`] record for a container adoption kept.
fn armed(job_id: &JobId, container: &AdoptedContainer) -> ArmedJob {
  ArmedJob {
    job_id: job_id.clone(),
    container_id: container.container_id.clone(),
    deadline_ms: container.deadline_ms,
  }
}

#[cfg(test)]
#[path = "tests/adopt.rs"]
mod tests;
