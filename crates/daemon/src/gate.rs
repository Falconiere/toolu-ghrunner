//! The daemon's resource gate: pure in-memory accounting for how many jobs
//! it is currently responsible for, and how much of its vCPU/memory budget
//! the running ones consume.
//!
//! toolu.sh delivers `workflow_job.queued` exactly once and has no requeue
//! path, so a 429 here permanently fails a customer's job (see
//! `crates/daemon/README.md`). Consequently the only thing this gate ever
//! refuses outright is the queue-depth ceiling (`TOOLU_DAEMON_QUEUE_MAX`) —
//! admission is a promise to run the job eventually, not a promise to run it
//! now. The vCPU/memory budget (`TOOLU_DAEMON_VCPU`/`TOOLU_DAEMON_MEMORY_MB`)
//! governs a separate question: whether an already-admitted job may *start*
//! right now. A job that fits nothing right now simply waits, tracked, until
//! another job finishes and releases its share.
//!
//! This module is pure accounting: no Docker, no HTTP, no locking. The
//! caller (the request handler, the exit reaper) owns thread-safety and
//! wires Docker `create`/`start`/exit events to `Gate::admit`,
//! `Gate::try_start` and `Gate::release`.

use std::collections::HashMap;
use std::fmt;

/// A resource footprint: whole vCPUs and memory in megabytes. Used both for
/// one job's size and for the gate's total budget — a budget is simply the
/// size of the box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JobSize {
  /// Whole vCPUs.
  pub vcpu: u32,
  /// Memory, in megabytes.
  pub memory_mb: u32,
}

impl JobSize {
  /// The empty footprint — nothing consumed.
  const ZERO: Self = Self {
    vcpu: 0,
    memory_mb: 0,
  };

  /// Add two footprints, clamping each dimension at `u32::MAX` instead of
  /// wrapping. Under correct use the sum of running jobs never legitimately
  /// approaches that ceiling — this is a defensive floor so a future bug
  /// fails safe ("no free capacity") instead of silently wrapping to a tiny
  /// used amount that would then over-admit.
  fn saturating_add(self, other: Self) -> Self {
    Self {
      vcpu: self.vcpu.saturating_add(other.vcpu),
      memory_mb: self.memory_mb.saturating_add(other.memory_mb),
    }
  }

  /// Subtract, clamping each dimension at zero instead of underflowing.
  /// `used` never legitimately exceeds the budget it was subtracted from —
  /// this is a defensive floor, not an expected path.
  fn saturating_sub(self, other: Self) -> Self {
    Self {
      vcpu: self.vcpu.saturating_sub(other.vcpu),
      memory_mb: self.memory_mb.saturating_sub(other.memory_mb),
    }
  }

  /// Whether this footprint fits within `budget` in both dimensions.
  fn fits_within(self, budget: Self) -> bool {
    self.vcpu <= budget.vcpu && self.memory_mb <= budget.memory_mb
  }
}

/// Identifies one job across [`Gate::admit`], [`Gate::try_start`] and
/// [`Gate::release`] — the id the caller already has for it (a Docker
/// container id in production, an arbitrary string in tests).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct JobId(String);

impl JobId {
  /// Build a job id from any string-like value.
  pub fn new(id: impl Into<String>) -> Self {
    Self(id.into())
  }

  /// The id as plain text — what `crate::docker::registry::JobRegistry` and
  /// `crate::reaper`'s container-side bookkeeping are keyed by, since
  /// neither depends on this type.
  pub fn as_str(&self) -> &str {
    &self.0
  }
}

/// Why [`Gate::admit`] refused a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmitError {
  /// The job's own footprint is larger than the gate's entire budget in
  /// some dimension — it could never start, so queuing it would wait
  /// forever. Rejected outright instead.
  ExceedsBudget,
  /// The queue already holds `queue_max` jobs. This is the only 429 this
  /// gate produces — see the module docs on why.
  QueueFull,
  /// `job_id` is already tracked; ids must be unique for the lifetime of
  /// the job they name.
  DuplicateJobId,
}

impl fmt::Display for AdmitError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::ExceedsBudget => write!(f, "job size exceeds the total budget"),
      Self::QueueFull => write!(f, "queue is at its depth ceiling"),
      Self::DuplicateJobId => write!(f, "job id is already tracked"),
    }
  }
}

impl std::error::Error for AdmitError {}

/// What [`Gate::try_start`] did with an admitted job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartOutcome {
  /// It fit the currently free budget and is now running.
  Started,
  /// Nothing free enough yet; it remains queued.
  Deferred,
}

/// A snapshot of what the gate currently holds — for startup adoption to
/// seed its own accounting against after a daemon restart, and for
/// observability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Consumption {
  /// vCPU consumed by running jobs.
  pub vcpu_used: u32,
  /// Memory, in megabytes, consumed by running jobs.
  pub memory_mb_used: u32,
  /// Jobs currently tracked — running and still-queued combined, the count
  /// the queue-depth ceiling bounds.
  pub queue_depth: u32,
}

/// One tracked job: its footprint, and whether it has been started.
#[derive(Debug, Clone, Copy)]
struct Entry {
  size: JobSize,
  running: bool,
}

/// Pure resource accounting for the daemon's admitted jobs. See the module
/// docs for the admission-vs-start distinction this exists to enforce.
#[derive(Debug, Clone)]
pub struct Gate {
  budget: JobSize,
  queue_max: u32,
  jobs: HashMap<JobId, Entry>,
}

impl Gate {
  /// Build a gate for a box with `budget` vCPU/memory, admitting at most
  /// `queue_max` jobs at a time.
  pub fn new(budget: JobSize, queue_max: u32) -> Self {
    Self {
      budget,
      queue_max,
      jobs: HashMap::new(),
    }
  }

  /// Admit `job_id` with footprint `size` into the queue.
  ///
  /// # Errors
  ///
  /// Returns [`AdmitError::DuplicateJobId`] if `job_id` is already tracked.
  /// Returns [`AdmitError::ExceedsBudget`] if `size` is larger than the
  /// gate's total budget in either dimension — such a job could never
  /// start, so it is rejected here rather than queued forever. Returns
  /// [`AdmitError::QueueFull`] if the queue is already at `queue_max` — the
  /// daemon's one and only 429.
  pub fn admit(&mut self, job_id: &JobId, size: JobSize) -> Result<(), AdmitError> {
    if self.jobs.contains_key(job_id) {
      return Err(AdmitError::DuplicateJobId);
    }
    if !size.fits_within(self.budget) {
      return Err(AdmitError::ExceedsBudget);
    }

    let depth = u32::try_from(self.jobs.len()).unwrap_or(u32::MAX);
    if depth >= self.queue_max {
      return Err(AdmitError::QueueFull);
    }

    self.jobs.insert(
      job_id.clone(),
      Entry {
        size,
        running: false,
      },
    );
    Ok(())
  }

  /// Seed `job_id` as a job that is **already running** and already
  /// consuming `size` — startup adoption's one way into the gate
  /// (`crate::adopt`).
  ///
  /// Unlike [`Self::admit`] this ignores both ceilings, because by the time
  /// it is called neither is a decision anymore: a container Docker reports
  /// running holds its vCPU and memory whether or not the current
  /// `TOOLU_DAEMON_VCPU`/`TOOLU_DAEMON_MEMORY_MB` would have admitted it, and
  /// declining to account for it is exactly the overcommit adoption exists to
  /// prevent — a restart that reset the budget to zero would then admit a
  /// whole box's worth of new work on top of what is already running. An
  /// adopted job that no longer fits simply leaves the gate over budget until
  /// it exits, and nothing else starts meanwhile.
  ///
  /// # Errors
  ///
  /// Returns [`AdmitError::DuplicateJobId`] if `job_id` is already tracked:
  /// two containers cannot both be the same job, and the caller decides which
  /// of them to remove.
  pub fn adopt_running(&mut self, job_id: &JobId, size: JobSize) -> Result<(), AdmitError> {
    if self.jobs.contains_key(job_id) {
      return Err(AdmitError::DuplicateJobId);
    }
    self.jobs.insert(
      job_id.clone(),
      Entry {
        size,
        running: true,
      },
    );
    Ok(())
  }

  /// Try to start `job_id` now: if its footprint fits the currently free
  /// budget it is marked running and consumes that budget; otherwise it
  /// stays queued. Idempotent — calling this on an already-running job just
  /// reports [`StartOutcome::Started`] without consuming budget twice.
  ///
  /// Returns `None` if `job_id` was never admitted, or has since been
  /// released.
  pub fn try_start(&mut self, job_id: &JobId) -> Option<StartOutcome> {
    let entry = *self.jobs.get(job_id)?;
    if entry.running {
      return Some(StartOutcome::Started);
    }

    let remaining = self.budget.saturating_sub(self.used());
    if !entry.size.fits_within(remaining) {
      return Some(StartOutcome::Deferred);
    }

    if let Some(tracked) = self.jobs.get_mut(job_id) {
      tracked.running = true;
    }
    Some(StartOutcome::Started)
  }

  /// Put an already-started job back in the "admitted, not running" state,
  /// giving its budget back while keeping its queue slot — what
  /// `crate::docker::DockerBackend::tick` does when the `docker start` it
  /// issued for a promoted job fails. [`Self::try_start`] marks a job running
  /// before the caller has actually started it, so without this a transient
  /// start failure would leave the gate believing a container is running that
  /// is not, and the next `crate::reaper::reconcile` pass would read that
  /// disagreement as an exit and remove a container the customer already has
  /// a 201 for.
  ///
  /// Returns `false` when `job_id` is not tracked at all — reaped, destroyed
  /// or released while the start was in flight — which tells the caller there
  /// is no job left to re-queue.
  pub fn unstart(&mut self, job_id: &JobId) -> bool {
    match self.jobs.get_mut(job_id) {
      Some(entry) => {
        entry.running = false;
        true
      },
      None => false,
    }
  }

  /// Whether the gate still tracks `job_id` at all, running or queued.
  ///
  /// `crate::routes::handlers` needs this to tell a create that finished
  /// normally from one whose job was reaped, destroyed or reconciled away
  /// while `docker create` was in flight: recording a container for a job the
  /// gate has already let go would leave two maps holding an entry nothing
  /// ever drains.
  pub fn tracks(&self, job_id: &JobId) -> bool {
    self.jobs.contains_key(job_id)
  }

  /// Release `job_id` — call this when its container exits, whether it
  /// finished normally or is being reaped after a daemon restart. Frees its
  /// queue slot and, if it had started, the budget it held.
  ///
  /// Returns the job's footprint, or `None` if `job_id` was not tracked.
  pub fn release(&mut self, job_id: &JobId) -> Option<JobSize> {
    self.jobs.remove(job_id).map(|entry| entry.size)
  }

  /// Job ids the gate currently believes are running — what
  /// `crate::reaper::reconcile` cross-references against Docker's own
  /// container list to notice an exit that happened without this daemon
  /// being told about it.
  pub fn running_job_ids(&self) -> Vec<JobId> {
    self
      .jobs
      .iter()
      .filter(|(_, entry)| entry.running)
      .map(|(job_id, _)| job_id.clone())
      .collect()
  }

  /// A snapshot of current consumption — see [`Consumption`].
  pub fn consumption(&self) -> Consumption {
    let used = self.used();
    let queue_depth = u32::try_from(self.jobs.len()).unwrap_or(u32::MAX);
    Consumption {
      vcpu_used: used.vcpu,
      memory_mb_used: used.memory_mb,
      queue_depth,
    }
  }

  /// Total footprint of every currently-running job.
  fn used(&self) -> JobSize {
    self
      .jobs
      .values()
      .filter(|entry| entry.running)
      .fold(JobSize::ZERO, |acc, entry| acc.saturating_add(entry.size))
  }
}

#[cfg(test)]
#[path = "tests/gate.rs"]
mod tests;
