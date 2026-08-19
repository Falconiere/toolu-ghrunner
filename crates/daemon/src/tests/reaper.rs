//! Tests for the reaper/scheduler's pure decision core: given a resource
//! gate, the create-tombstone registry, the start queue and the
//! created-container map, which jobs [`reconcile`] starts, kills and
//! releases — over real tag-catalog sizes
//! (`packages/api/src/github/runner-tags.ts`, toolu.sh repo) and a real
//! epoch-millisecond deadline shape. Nothing here touches Docker:
//! `reconcile` takes a snapshot of what Docker would report as a plain
//! argument, and the clock as another, so every scenario is deterministic
//! without a daemon.

use std::time::Instant;

use super::{ContainerSnapshot, CreatedContainers, StartQueue, reconcile};
use crate::docker::registry::{BeginOutcome, FinishOutcome, JobRegistry};
use crate::gate::{AdmitError, Gate, JobId, JobSize, StartOutcome};

/// `toolu-ubuntu` / `toolu-ubuntu-2vcpu-4gb` — the Linux floor.
const TAG_2VCPU_4GB: JobSize = JobSize {
  vcpu: 2,
  memory_mb: 4096,
};
/// `toolu-ubuntu-8vcpu-16gb`.
const TAG_8VCPU_16GB: JobSize = JobSize {
  vcpu: 8,
  memory_mb: 16384,
};
/// `toolu-ubuntu-8vcpu-32gb`.
const TAG_8VCPU_32GB: JobSize = JobSize {
  vcpu: 8,
  memory_mb: 32768,
};
/// `toolu-ubuntu-16vcpu-32gb` — the largest Linux tag toolu.sh can send;
/// used as the whole box's budget so the two 8-vcpu tags above genuinely
/// compete for it.
const TAG_16VCPU_32GB: JobSize = JobSize {
  vcpu: 16,
  memory_mb: 32768,
};

/// A real epoch-millisecond deadline shape: `nowMs` plus six hours, what
/// `client.ts` sends (`crates/daemon/src/tests/docker.rs`'s `DEADLINE_MS`).
const FAR_FUTURE_DEADLINE_MS: i64 = 1_763_000_000_000;
/// A tick one second before the deadline — still live.
const JUST_BEFORE_DEADLINE_MS: i64 = FAR_FUTURE_DEADLINE_MS - 1_000;
/// A tick one second after the deadline — expired.
const JUST_AFTER_DEADLINE_MS: i64 = FAR_FUTURE_DEADLINE_MS + 1_000;

#[test]
fn an_exit_frees_exactly_its_own_budget_and_promotes_a_waiting_job() {
  let mut gate = Gate::new(TAG_16VCPU_32GB, 8);
  let mut registry = JobRegistry::new();
  let mut queue = StartQueue::new();
  let mut created = CreatedContainers::new();

  let job_a = JobId::new("job-a");
  let job_b = JobId::new("job-b");

  // Both admitted at request time and both containers created — exactly
  // what `create_job` does before either has a chance to start.
  assert!(gate.admit(&job_a, TAG_8VCPU_16GB).is_ok());
  assert!(gate.admit(&job_b, TAG_8VCPU_32GB).is_ok());
  queue.push(job_a.clone());
  queue.push(job_b.clone());
  created.record("job-a", "container-a");
  created.record("job-b", "container-b");

  // First tick: neither has started, so Docker reports both created but not
  // running. A fits the whole free budget and starts; B does not fit what
  // is left over (8vcpu/16gb free; B needs 8vcpu/32gb) and stays queued.
  let snapshot = vec![
    ContainerSnapshot::new("job-a", "container-a", FAR_FUTURE_DEADLINE_MS, false),
    ContainerSnapshot::new("job-b", "container-b", FAR_FUTURE_DEADLINE_MS, false),
  ];
  let first_tick = reconcile(
    &mut gate,
    &mut registry,
    &mut queue,
    &mut created,
    &snapshot,
    JUST_BEFORE_DEADLINE_MS,
  );
  assert_eq!(first_tick.start, vec!["container-a".to_owned()]);
  assert!(first_tick.remove.is_empty());
  assert_eq!(gate.running_job_ids(), vec![job_a.clone()]);
  assert_eq!(queue.len(), 1);

  // Second tick: A's container has since exited on its own — the common
  // case `crates/daemon/README.md` calls out, with no deadline ever in
  // play. Its budget frees, and B — the job that was waiting — starts with
  // exactly that freed room.
  let snapshot = vec![
    ContainerSnapshot::new("job-a", "container-a", FAR_FUTURE_DEADLINE_MS, false),
    ContainerSnapshot::new("job-b", "container-b", FAR_FUTURE_DEADLINE_MS, false),
  ];
  let second_tick = reconcile(
    &mut gate,
    &mut registry,
    &mut queue,
    &mut created,
    &snapshot,
    JUST_BEFORE_DEADLINE_MS,
  );
  assert_eq!(second_tick.remove, vec!["container-a".to_owned()]);
  assert_eq!(second_tick.start, vec!["container-b".to_owned()]);
  assert_eq!(gate.running_job_ids(), vec![job_b]);
  assert!(queue.is_empty());
  assert_eq!(gate.consumption().vcpu_used, 8);
  assert_eq!(gate.consumption().memory_mb_used, 32768);
  assert_eq!(created.existing("job-a"), None);
}

#[test]
fn a_job_past_its_deadline_is_killed_and_released() {
  let mut gate = Gate::new(TAG_16VCPU_32GB, 8);
  let mut registry = JobRegistry::new();
  let mut queue = StartQueue::new();
  let mut created = CreatedContainers::new();

  let job = JobId::new("job-timeout");
  assert!(gate.admit(&job, TAG_2VCPU_4GB).is_ok());
  assert_eq!(gate.try_start(&job), Some(StartOutcome::Started));
  created.record("job-timeout", "container-timeout");

  // Still running when the deadline catches it — the box did not free the
  // budget on its own, so the tick has to.
  let snapshot = vec![ContainerSnapshot::new(
    "job-timeout",
    "container-timeout",
    FAR_FUTURE_DEADLINE_MS,
    true,
  )];
  let outcome = reconcile(
    &mut gate,
    &mut registry,
    &mut queue,
    &mut created,
    &snapshot,
    JUST_AFTER_DEADLINE_MS,
  );

  assert_eq!(outcome.remove, vec!["container-timeout".to_owned()]);
  assert!(outcome.start.is_empty());
  assert!(gate.running_job_ids().is_empty());
  assert_eq!(gate.consumption().queue_depth, 0);
  assert_eq!(created.existing("job-timeout"), None);
}

#[test]
fn a_job_before_its_deadline_is_untouched() {
  let mut gate = Gate::new(TAG_16VCPU_32GB, 8);
  let mut registry = JobRegistry::new();
  let mut queue = StartQueue::new();
  let mut created = CreatedContainers::new();

  let job = JobId::new("job-on-time");
  assert!(gate.admit(&job, TAG_2VCPU_4GB).is_ok());
  assert_eq!(gate.try_start(&job), Some(StartOutcome::Started));
  created.record("job-on-time", "container-on-time");

  let snapshot = vec![ContainerSnapshot::new(
    "job-on-time",
    "container-on-time",
    FAR_FUTURE_DEADLINE_MS,
    true,
  )];
  let outcome = reconcile(
    &mut gate,
    &mut registry,
    &mut queue,
    &mut created,
    &snapshot,
    JUST_BEFORE_DEADLINE_MS,
  );

  assert!(outcome.remove.is_empty());
  assert!(outcome.start.is_empty());
  assert_eq!(gate.running_job_ids(), vec![job]);
  assert_eq!(created.existing("job-on-time"), Some("container-on-time"));
}

#[test]
fn a_repeated_create_is_idempotent() {
  let mut gate = Gate::new(TAG_16VCPU_32GB, 8);
  let mut created = CreatedContainers::new();

  let job = JobId::new("job-redelivered");
  assert!(gate.admit(&job, TAG_2VCPU_4GB).is_ok());
  created.record("job-redelivered", "container-once");
  assert_eq!(gate.consumption().queue_depth, 1);

  // GitHub redelivers the same webhook: the gate refuses the second
  // admission outright — real and benign, not a fault (`AC`s aside, this is
  // exactly the case that used to reach the client as a 500).
  assert_eq!(
    gate.admit(&job, TAG_2VCPU_4GB),
    Err(AdmitError::DuplicateJobId)
  );
  // No second slot consumed…
  assert_eq!(gate.consumption().queue_depth, 1);
  // …and the daemon holds exactly the container id to answer the repeat
  // with — the same one the first, successful create produced.
  assert_eq!(created.existing("job-redelivered"), Some("container-once"));
}

#[test]
fn the_tombstone_is_forgotten_on_exit() {
  let mut gate = Gate::new(TAG_16VCPU_32GB, 8);
  let mut registry = JobRegistry::new();
  let mut queue = StartQueue::new();
  let mut created = CreatedContainers::new();

  let job = JobId::new("job-finishes");
  assert!(gate.admit(&job, TAG_2VCPU_4GB).is_ok());
  assert_eq!(gate.try_start(&job), Some(StartOutcome::Started));
  created.record("job-finishes", "container-finishes");

  let now = Instant::now();
  assert_eq!(
    registry.begin_create("job-finishes", now),
    BeginOutcome::Proceed
  );
  assert_eq!(
    registry.finish_create("job-finishes", "container-finishes", now),
    FinishOutcome::Keep
  );
  assert_eq!(
    registry.job_for_container("container-finishes"),
    Some("job-finishes".to_owned())
  );

  let snapshot = vec![ContainerSnapshot::new(
    "job-finishes",
    "container-finishes",
    FAR_FUTURE_DEADLINE_MS,
    false,
  )];
  let outcome = reconcile(
    &mut gate,
    &mut registry,
    &mut queue,
    &mut created,
    &snapshot,
    JUST_BEFORE_DEADLINE_MS,
  );

  assert_eq!(outcome.remove, vec!["container-finishes".to_owned()]);
  assert_eq!(registry.job_for_container("container-finishes"), None);
}

#[test]
fn a_job_that_vanishes_entirely_is_still_released() {
  // Not in Docker's list at all — removed out from under the daemon, or
  // lost across a restart this daemon has not adopted from yet. The gate
  // still believes it is running, so the tick must notice and release it
  // even with no container id left to act on.
  let mut gate = Gate::new(TAG_16VCPU_32GB, 8);
  let mut registry = JobRegistry::new();
  let mut queue = StartQueue::new();
  let mut created = CreatedContainers::new();

  let job = JobId::new("job-vanished");
  assert!(gate.admit(&job, TAG_2VCPU_4GB).is_ok());
  assert_eq!(gate.try_start(&job), Some(StartOutcome::Started));
  created.record("job-vanished", "container-vanished");

  let outcome = reconcile(
    &mut gate,
    &mut registry,
    &mut queue,
    &mut created,
    &[],
    JUST_BEFORE_DEADLINE_MS,
  );

  assert!(outcome.remove.is_empty());
  assert!(gate.running_job_ids().is_empty());
  assert_eq!(created.existing("job-vanished"), None);
}
