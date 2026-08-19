//! The daemon's reaper and scheduler: pure decision logic for the three
//! duties `crates/daemon/README.md` assigns the process once a job's
//! container exists — release its budget when it exits, start whatever the
//! freed budget now promotes, and kill anything that outlived its
//! `sh.toolu.deadline` label.
//!
//! **The common case is exit, not deadline.** A runner finishes its one job
//! and exits on its own; the deadline label exists for the case where it
//! does not. Both are handled by the same function, [`reconcile`], because
//! both end in the identical bookkeeping: give the job's budget back, drop
//! every trace this daemon kept of it, and see whether that freed enough
//! room for something still waiting.
//!
//! Everything here is pure — no bollard, no Docker socket, no clock of its
//! own. [`reconcile`] takes a snapshot of what Docker currently reports
//! (built fresh each call — state lives in Docker, not in this process, per
//! `crates/daemon/README.md`) and the current time as plain arguments, so
//! every decision is deterministic and testable without a daemon. The
//! caller — `crate::docker::DockerBackend::tick` — is what actually talks
//! to Docker: listing containers into a snapshot beforehand, then acting on
//! the [`TickOutcome`] this module hands back.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::docker::registry::JobRegistry;
use crate::gate::{Gate, JobId, StartOutcome};

/// FIFO order of admitted jobs whose containers exist but have not yet
/// started. [`Gate`] accounts vCPU/memory but its internal map carries no
/// order of its own, so promotion needs this alongside it to be fair: the
/// job that has been waiting longest starts first once budget frees.
#[derive(Debug, Default)]
pub struct StartQueue(VecDeque<JobId>);

impl StartQueue {
  /// An empty queue.
  pub fn new() -> Self {
    Self(VecDeque::new())
  }

  /// Record `job_id` as waiting to start, behind whatever is already
  /// queued.
  pub fn push(&mut self, job_id: JobId) {
    self.0.push_back(job_id);
  }

  /// Drop `job_id` from the queue, wherever it is — a no-op if it is not
  /// there, which is the common case once it has started or was never
  /// queued to begin with.
  pub fn remove(&mut self, job_id: &JobId) {
    self.0.retain(|queued| queued != job_id);
  }

  /// How many jobs are currently waiting to start.
  pub fn len(&self) -> usize {
    self.0.len()
  }

  /// Whether nothing is currently waiting to start.
  pub fn is_empty(&self) -> bool {
    self.0.is_empty()
  }
}

/// The container id recorded for each job whose `docker create` has
/// finished, keyed by GitHub's job id.
///
/// This is what makes a redelivered `POST /v1/jobs` for the same job id
/// idempotent: `crate::routes::handlers::create_job` consults it when the
/// gate reports a duplicate admission, answering with the same container id
/// the first, successful call already produced rather than calling `docker
/// create` again. [`reconcile`] forgets an entry the instant its job's
/// budget is released, so the map never outlives the job it describes.
#[derive(Debug, Default)]
pub struct CreatedContainers(HashMap<String, String>);

impl CreatedContainers {
  /// An empty map.
  pub fn new() -> Self {
    Self(HashMap::new())
  }

  /// Record that `job_id`'s `docker create` produced `container_id`.
  pub fn record(&mut self, job_id: &str, container_id: &str) {
    self.0.insert(job_id.to_owned(), container_id.to_owned());
  }

  /// The container id recorded for `job_id`, if its create has finished.
  pub fn existing(&self, job_id: &str) -> Option<&str> {
    self.0.get(job_id).map(String::as_str)
  }

  /// Drop the entry for `job_id` — called wherever its gate entry is
  /// released, so the two never drift apart.
  pub fn forget(&mut self, job_id: &str) {
    self.0.remove(job_id);
  }
}

/// One container this daemon created, as read back from Docker at the start
/// of a [`reconcile`] tick: its identity and deadline from the
/// `sh.toolu.job-id`/`sh.toolu.deadline` labels (`crate::docker::spec`,
/// never `Config.Env` — that block also carries the JIT config), and
/// whether Docker currently reports it running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerSnapshot {
  /// GitHub's own job id, from the `sh.toolu.job-id` label.
  pub job_id: JobId,
  /// The container's id.
  pub container_id: String,
  /// Epoch milliseconds the job must finish by, from the
  /// `sh.toolu.deadline` label.
  pub deadline_ms: i64,
  /// Whether Docker currently reports this container running.
  pub running: bool,
}

impl ContainerSnapshot {
  /// Build a snapshot entry for `job_id`'s `container_id`.
  pub fn new(
    job_id: impl Into<String>,
    container_id: impl Into<String>,
    deadline_ms: i64,
    running: bool,
  ) -> Self {
    Self {
      job_id: JobId::new(job_id),
      container_id: container_id.into(),
      deadline_ms,
      running,
    }
  }
}

/// One container the tick promoted, and the job it serves.
///
/// The job id travels with the container id because a `docker start` can
/// fail — [`promote`] has already marked the job running in the gate and
/// taken it out of the queue by the time the caller issues that start, so a
/// failure has to be able to put both back. Without the job id the caller
/// would hold a container id it cannot map to a gate entry, and the next
/// [`reconcile`] pass would read the gate's "running" against Docker's "not
/// running" as an exit and remove a container the customer already has a 201
/// for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartRequest {
  /// GitHub's own job id — the gate's and the start queue's key.
  pub job_id: JobId,
  /// The container to `docker start`.
  pub container_id: String,
}

/// What one [`reconcile`] tick decided, in terms the caller executes
/// against Docker.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TickOutcome {
  /// Container ids to force-remove: exited on their own, or outlived their
  /// deadline while still running.
  pub remove: Vec<String>,
  /// Containers to `docker start`, in the order they should be started —
  /// budget this tick freed now covers them.
  pub start: Vec<StartRequest>,
}

/// Try to start every job in `queue`'s arrival order against `gate`'s
/// currently free budget. Started jobs are removed from `queue`; jobs that
/// still do not fit stay, unchanged, for the next call.
fn promote(gate: &mut Gate, queue: &mut StartQueue) -> Vec<JobId> {
  let pending: Vec<JobId> = queue.0.iter().cloned().collect();
  let mut started = Vec::new();
  for job_id in pending {
    if matches!(gate.try_start(&job_id), Some(StartOutcome::Started)) {
      queue.remove(&job_id);
      started.push(job_id);
    }
  }
  started
}

/// Drop every trace of `job_id`: its gate budget, its reap tombstone, its
/// place in the start queue (if it never started), and its recorded
/// container id. Returns the freed footprint, or `None` if `job_id` was not
/// tracked — releasing an id twice is a no-op, not an error.
///
/// Public because [`reconcile`] is not the only place a job stops being this
/// daemon's responsibility: `crate::docker::DockerBackend::tick` calls it for
/// a promoted job whose container Docker no longer has, which no later
/// snapshot can ever mention again and which would otherwise hold its queue
/// slot forever.
pub fn release_job(
  gate: &mut Gate,
  registry: &mut JobRegistry,
  queue: &mut StartQueue,
  created: &mut CreatedContainers,
  job_id: &str,
) -> Option<crate::gate::JobSize> {
  let released = gate.release(&JobId::new(job_id));
  registry.forget(job_id);
  created.forget(job_id);
  queue.remove(&JobId::new(job_id));
  released
}

/// One reconciliation tick: the daemon's whole reaper-and-scheduler duty in
/// a single call, driven entirely off `snapshot` (what Docker reports right
/// now) and `now_ms` (the caller's clock, passed in rather than read here so
/// deadline decisions are deterministic in tests).
///
/// Three passes, in order:
///
/// 1. **Deadline kills.** Every entry in `snapshot` whose `deadline_ms` has
///    passed is released and queued for removal — running or not, since a
///    job that never got budget before its deadline is moot to GitHub
///    either way.
/// 2. **Exit release.** A job the gate still marks running whose container
///    `snapshot` no longer reports running (or does not mention at all —
///    vanished out from under the daemon) is released too. This is the
///    common case from `crates/daemon/README.md`: a runner finishes its one
///    job and exits on its own, with no deadline ever in play.
/// 3. **Promotion.** Whatever budget the first two passes freed is offered
///    to the start queue in arrival order; jobs that fit are reported for
///    `docker start`.
pub fn reconcile(
  gate: &mut Gate,
  registry: &mut JobRegistry,
  queue: &mut StartQueue,
  created: &mut CreatedContainers,
  snapshot: &[ContainerSnapshot],
  now_ms: i64,
) -> TickOutcome {
  let running_before: HashSet<JobId> = gate.running_job_ids().into_iter().collect();
  let mut remove = Vec::new();

  for entry in snapshot {
    let deadline_passed = entry.deadline_ms <= now_ms;
    let exited_on_its_own = !entry.running && running_before.contains(&entry.job_id);
    if deadline_passed || exited_on_its_own {
      // Removed regardless of whether the gate had this job tracked: a
      // container Docker itself reports past its own deadline label is
      // unambiguously ours to remove, even if this process never admitted
      // it (adopted from a restart it has not caught up on yet, or a bug
      // elsewhere left it untracked). `release_job` is a safe no-op when
      // there is nothing to release.
      release_job(gate, registry, queue, created, entry.job_id.as_str());
      remove.push(entry.container_id.clone());
    }
  }

  // A job the gate still marks running whose container never appeared in
  // `snapshot` at all has vanished entirely — released too, though there is
  // no container id left to act on.
  let seen: HashSet<&JobId> = snapshot.iter().map(|entry| &entry.job_id).collect();
  for job_id in running_before {
    if !seen.contains(&job_id) {
      release_job(gate, registry, queue, created, job_id.as_str());
    }
  }

  let promoted = promote(gate, queue);
  let start = promoted
    .into_iter()
    .filter_map(|job_id| {
      let container_id = created.existing(job_id.as_str())?.to_owned();
      Some(StartRequest {
        job_id,
        container_id,
      })
    })
    .collect();

  TickOutcome { remove, start }
}

#[cfg(test)]
#[path = "tests/reaper.rs"]
mod tests;
