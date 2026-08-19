//! The in-flight job registry, and the tombstone that makes
//! `DELETE /v1/jobs?jobId=…` able to cancel a create that has not finished.
//!
//! The reap route is the client's recovery path: when `POST /v1/jobs` times
//! out at 10 seconds the container's fate is unknown, so it reaps by job id.
//! A container that does not exist *yet* carries no label, so a reap that
//! only asked Docker would no-op — the daemon would then finish creating,
//! start a runner against a JIT config toolu has already marked `failed`, and
//! that runner would serve a real job for up to six hours with no destroy
//! handle. Recording the job id before the first Docker call, and refusing to
//! keep a container whose job was reaped meanwhile, is what closes that.
//!
//! Pure state: no Docker, no HTTP, no clock of its own — the caller passes
//! `now`, so every transition here is testable without a daemon.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How long a reap tombstone for a job id with no container is kept.
///
/// It only has to outlive a create that is already in flight — seconds — but
/// it must be bounded: `vps/verify.ts` (toolu.sh repo) probes a host's
/// credentials by reaping a sentinel job id that matches nothing, so
/// tombstones would otherwise accumulate one per credential check, forever.
pub const TOMBSTONE_TTL: Duration = Duration::from_secs(600);

/// What the daemon knows about one job id right now.
#[derive(Debug, Clone, PartialEq, Eq)]
enum JobState {
  /// A `docker create` for this job is in flight; no container id yet.
  Creating,
  /// The container exists. It is not started here — the resource gate
  /// governs starting.
  Created {
    /// The created container's id.
    container_id: String,
  },
  /// The job was reaped. Any container that lands for it afterwards must be
  /// removed rather than started.
  Reaped {
    /// When the reap arrived, for [`TOMBSTONE_TTL`] expiry.
    at: Instant,
  },
}

/// What [`JobRegistry::begin_create`] decided about a create that is about to
/// call Docker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BeginOutcome {
  /// Nothing else holds this job id: the create may call Docker.
  Proceed,
  /// A reap for this job id already arrived. The create must not run at all —
  /// no container, no Docker call.
  AlreadyReaped,
  /// Another create for this job id is still in flight, or its container
  /// already exists.
  AlreadyTracked,
}

/// What [`JobRegistry::finish_create`] decided about a container that has
/// just been created.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishOutcome {
  /// Nothing happened meanwhile: keep the container.
  Keep,
  /// The job was reaped while the create was in flight. The container must be
  /// removed and must never start.
  DiscardReaped,
}

/// Every job id this daemon is currently responsible for, plus the tombstones
/// of the ones reaped recently. See the module docs.
#[derive(Debug, Default)]
pub struct JobRegistry {
  /// Keyed by GitHub's job id — the same key the reap route addresses and the
  /// `sh.toolu.job-id` label carries.
  jobs: HashMap<String, JobState>,
}

impl JobRegistry {
  /// An empty registry. Startup adoption seeds a real one from container
  /// labels.
  pub fn new() -> Self {
    Self::default()
  }

  /// Claim `job_id` for a create that is about to call Docker. Call this
  /// **before** the first Docker call: everything the reap route can address
  /// depends on the id being recorded first.
  pub fn begin_create(&mut self, job_id: &str, now: Instant) -> BeginOutcome {
    self.prune(now);
    match self.jobs.get(job_id) {
      Some(JobState::Reaped { .. }) => BeginOutcome::AlreadyReaped,
      Some(JobState::Creating | JobState::Created { .. }) => BeginOutcome::AlreadyTracked,
      None => {
        self.jobs.insert(job_id.to_owned(), JobState::Creating);
        BeginOutcome::Proceed
      },
    }
  }

  /// Record that `job_id`'s container now exists, and report whether it may
  /// be kept — a reap that arrived mid-create yields
  /// [`FinishOutcome::DiscardReaped`] and leaves the tombstone standing.
  pub fn finish_create(&mut self, job_id: &str, container_id: &str, now: Instant) -> FinishOutcome {
    match self.jobs.get(job_id) {
      Some(JobState::Reaped { .. }) => {
        // Refresh the tombstone rather than dropping it: the container is
        // about to be removed, and a retry arriving in that window must not
        // slip past the reap either.
        self
          .jobs
          .insert(job_id.to_owned(), JobState::Reaped { at: now });
        FinishOutcome::DiscardReaped
      },
      Some(JobState::Creating | JobState::Created { .. }) | None => {
        self.jobs.insert(
          job_id.to_owned(),
          JobState::Created {
            container_id: container_id.to_owned(),
          },
        );
        FinishOutcome::Keep
      },
    }
  }

  /// Give up a claim taken by [`Self::begin_create`] whose Docker call
  /// failed, so a later delivery of the same job can try again. A tombstone
  /// is left alone — a reaped job stays reaped.
  pub fn abandon_create(&mut self, job_id: &str) {
    let is_claim = matches!(
      self.jobs.get(job_id),
      Some(JobState::Creating | JobState::Created { .. })
    );
    if is_claim {
      self.jobs.remove(job_id);
    }
  }

  /// Mark `job_id` reaped and report the container to remove, if one already
  /// exists. A job id this registry has never seen still gets a tombstone:
  /// that is the case where the reap has overtaken the create it addresses.
  pub fn reap(&mut self, job_id: &str, now: Instant) -> Option<String> {
    self.prune(now);
    let previous = self
      .jobs
      .insert(job_id.to_owned(), JobState::Reaped { at: now });
    match previous {
      Some(JobState::Created { container_id }) => Some(container_id),
      Some(JobState::Creating | JobState::Reaped { .. }) | None => None,
    }
  }

  /// Drop everything tracked for `job_id` — its container is gone and its
  /// budget released, so a future job with the same id starts clean.
  pub fn forget(&mut self, job_id: &str) {
    self.jobs.remove(job_id);
  }

  /// The job id serving `container_id`, if this process created it. Startup
  /// adoption apart, the `sh.toolu.job-id` label is the authority — this is
  /// the in-process shortcut, and a restart legitimately answers `None`.
  pub fn job_for_container(&self, container_id: &str) -> Option<String> {
    self.jobs.iter().find_map(|(job_id, state)| match state {
      JobState::Created {
        container_id: known,
      } if known == container_id => Some(job_id.clone()),
      JobState::Created { .. } | JobState::Creating | JobState::Reaped { .. } => None,
    })
  }

  /// Drop tombstones older than [`TOMBSTONE_TTL`]. Live jobs never expire by
  /// time: a job is forgotten when its container is gone, not when it is old.
  fn prune(&mut self, now: Instant) {
    self.jobs.retain(|_job_id, state| match state {
      JobState::Reaped { at } => now.saturating_duration_since(*at) < TOMBSTONE_TTL,
      JobState::Creating | JobState::Created { .. } => true,
    });
  }
}
