//! Tests for the resource gate. Job sizes are the real Linux tags from
//! toolu.sh's catalog (`packages/api/src/github/runner-tags.ts`) — not
//! invented numbers — except in `no_overflow_at_boundary_values`, which
//! needs sizes near `u32::MAX` that no real tag ever reaches.

use super::{AdmitError, Gate, JobId, JobSize, StartOutcome};

/// `toolu-ubuntu` / `toolu-ubuntu-2vcpu-4gb` — the Linux floor.
const TAG_2VCPU_4GB: JobSize = JobSize {
  vcpu: 2,
  memory_mb: 4096,
};
/// `toolu-ubuntu-4vcpu-8gb`.
const TAG_4VCPU_8GB: JobSize = JobSize {
  vcpu: 4,
  memory_mb: 8192,
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
/// `toolu-ubuntu-16vcpu-32gb` — the largest Linux tag toolu.sh can send.
const TAG_16VCPU_32GB: JobSize = JobSize {
  vcpu: 16,
  memory_mb: 32768,
};

#[test]
fn admits_up_to_the_queue_ceiling_then_rejects() {
  // Budget generous enough that sizing never interferes with this test —
  // only queue depth is under test.
  let budget = JobSize {
    vcpu: 64,
    memory_mb: 131_072,
  };
  let mut gate = Gate::new(budget, 3);

  let a = JobId::new("job-a");
  let b = JobId::new("job-b");
  let c = JobId::new("job-c");
  let d = JobId::new("job-d");

  assert!(gate.admit(&a, TAG_2VCPU_4GB).is_ok());
  assert!(gate.admit(&b, TAG_2VCPU_4GB).is_ok());
  assert!(gate.admit(&c, TAG_2VCPU_4GB).is_ok());
  assert_eq!(gate.consumption().queue_depth, 3);

  let rejection = gate.admit(&d, TAG_2VCPU_4GB);
  assert_eq!(rejection, Err(AdmitError::QueueFull));
  assert_eq!(gate.consumption().queue_depth, 3);
}

#[test]
fn duplicate_job_id_is_rejected() {
  let budget = JobSize {
    vcpu: 16,
    memory_mb: 32768,
  };
  let mut gate = Gate::new(budget, 8);
  let a = JobId::new("job-a");

  assert!(gate.admit(&a, TAG_2VCPU_4GB).is_ok());
  assert_eq!(
    gate.admit(&a, TAG_2VCPU_4GB),
    Err(AdmitError::DuplicateJobId)
  );
}

#[test]
fn budget_admits_a_fitting_job_and_defers_one_that_does_not_then_release_unblocks_it() {
  // Budget is exactly one `toolu-ubuntu-16vcpu-32gb` box.
  let budget = TAG_16VCPU_32GB;
  let mut gate = Gate::new(budget, 8);

  let a = JobId::new("job-a");
  let b = JobId::new("job-b");

  // A (8vcpu/16gb) is admitted and starts immediately: the whole budget is
  // free.
  assert!(gate.admit(&a, TAG_8VCPU_16GB).is_ok());
  assert_eq!(gate.try_start(&a), Some(StartOutcome::Started));

  // B (8vcpu/32gb) is admitted — it fits the TOTAL budget (8<=16,
  // 32768<=32768) so admission succeeds — but not the budget remaining
  // after A (8vcpu/16384mb free; B needs 32768mb), so it is deferred and
  // stays queued rather than refused.
  assert!(gate.admit(&b, TAG_8VCPU_32GB).is_ok());
  assert_eq!(gate.try_start(&b), Some(StartOutcome::Deferred));

  let consumption = gate.consumption();
  assert_eq!(consumption.vcpu_used, 8);
  assert_eq!(consumption.memory_mb_used, 16384);
  assert_eq!(consumption.queue_depth, 2);

  // Releasing A frees exactly what it held, and the whole budget is free
  // again — enough for B to start.
  let freed = gate.release(&a);
  assert_eq!(freed, Some(TAG_8VCPU_16GB));
  assert_eq!(gate.try_start(&b), Some(StartOutcome::Started));

  let consumption = gate.consumption();
  assert_eq!(consumption.vcpu_used, 8);
  assert_eq!(consumption.memory_mb_used, 32768);
  assert_eq!(consumption.queue_depth, 1);
}

#[test]
fn job_larger_than_the_whole_budget_is_rejected_at_admission_not_queued() {
  // Budget smaller than the largest Linux tag in every dimension.
  let budget = TAG_8VCPU_16GB;
  let mut gate = Gate::new(budget, 8);
  let a = JobId::new("job-a");

  let result = gate.admit(&a, TAG_16VCPU_32GB);
  assert_eq!(result, Err(AdmitError::ExceedsBudget));

  // Not tracked at all — it never occupies a queue slot.
  assert_eq!(gate.consumption().queue_depth, 0);
  assert_eq!(gate.try_start(&a), None);
  assert_eq!(gate.release(&a), None);
}

#[test]
fn a_job_matching_the_budget_exactly_is_admitted_and_can_start() {
  let budget = TAG_16VCPU_32GB;
  let mut gate = Gate::new(budget, 1);
  let a = JobId::new("job-a");

  assert!(gate.admit(&a, TAG_16VCPU_32GB).is_ok());
  assert_eq!(gate.try_start(&a), Some(StartOutcome::Started));
}

#[test]
fn consumption_reporting_is_exact_after_a_mixed_sequence_of_admits_and_releases() {
  // Budget holds exactly one 2vcpu/4gb job plus one 4vcpu/8gb job at once.
  let budget = JobSize {
    vcpu: 6,
    memory_mb: 12288,
  };
  let mut gate = Gate::new(budget, 5);

  let a = JobId::new("job-a"); // TAG_2VCPU_4GB
  let b = JobId::new("job-b"); // TAG_4VCPU_8GB
  let c = JobId::new("job-c"); // TAG_2VCPU_4GB, admitted but deferred

  assert!(gate.admit(&a, TAG_2VCPU_4GB).is_ok());
  assert_eq!(gate.try_start(&a), Some(StartOutcome::Started));

  assert!(gate.admit(&b, TAG_4VCPU_8GB).is_ok());
  assert_eq!(gate.try_start(&b), Some(StartOutcome::Started));

  // Budget is now fully consumed (6vcpu/12288mb of 6vcpu/12288mb): C is
  // admitted (it fits the total budget) but cannot start yet.
  assert!(gate.admit(&c, TAG_2VCPU_4GB).is_ok());
  assert_eq!(gate.try_start(&c), Some(StartOutcome::Deferred));

  let consumption = gate.consumption();
  assert_eq!(consumption.vcpu_used, 6);
  assert_eq!(consumption.memory_mb_used, 12288);
  assert_eq!(consumption.queue_depth, 3);

  // Releasing A frees exactly its own footprint — enough for the queued C
  // to start, but B's accounting is untouched.
  assert_eq!(gate.release(&a), Some(TAG_2VCPU_4GB));
  assert_eq!(gate.try_start(&c), Some(StartOutcome::Started));

  let consumption = gate.consumption();
  assert_eq!(consumption.vcpu_used, 6);
  assert_eq!(consumption.memory_mb_used, 12288);
  assert_eq!(consumption.queue_depth, 2);

  // Draining the rest brings consumption back to exactly zero.
  assert_eq!(gate.release(&b), Some(TAG_4VCPU_8GB));
  assert_eq!(gate.release(&c), Some(TAG_2VCPU_4GB));

  let consumption = gate.consumption();
  assert_eq!(consumption.vcpu_used, 0);
  assert_eq!(consumption.memory_mb_used, 0);
  assert_eq!(consumption.queue_depth, 0);
}

#[test]
fn releasing_an_unknown_job_is_a_no_op() {
  let budget = TAG_16VCPU_32GB;
  let mut gate = Gate::new(budget, 4);
  let ghost = JobId::new("never-admitted");

  assert_eq!(gate.release(&ghost), None);
  assert_eq!(gate.consumption().queue_depth, 0);
}

#[test]
fn no_overflow_at_boundary_values() {
  // Synthetic values at the very top of u32's range — no real tag reaches
  // this, this exists solely to prove the internal accounting is
  // saturating rather than wrapping. Two running jobs whose vCPU shares
  // sum to exactly `u32::MAX`: a wrapping add here would report a tiny
  // (wrapped) used amount instead, which would silently over-admit.
  let budget = JobSize {
    vcpu: u32::MAX,
    memory_mb: 65536,
  };
  let mut gate = Gate::new(budget, 2);

  let a = JobId::new("huge-a");
  let b = JobId::new("huge-b");
  let a_size = JobSize {
    vcpu: u32::MAX - 16,
    memory_mb: 32768,
  };
  let b_size = JobSize {
    vcpu: 16,
    memory_mb: 32768,
  };

  assert!(gate.admit(&a, a_size).is_ok());
  assert_eq!(gate.try_start(&a), Some(StartOutcome::Started));

  assert!(gate.admit(&b, b_size).is_ok());
  assert_eq!(gate.try_start(&b), Some(StartOutcome::Started));

  let consumption = gate.consumption();
  assert_eq!(consumption.vcpu_used, u32::MAX);
  assert_eq!(consumption.memory_mb_used, 65536);
  assert_eq!(consumption.queue_depth, 2);

  // Releasing both drains back to exactly zero, not a wrapped garbage
  // value.
  assert_eq!(gate.release(&a), Some(a_size));
  assert_eq!(gate.release(&b), Some(b_size));
  let consumption = gate.consumption();
  assert_eq!(consumption.vcpu_used, 0);
  assert_eq!(consumption.memory_mb_used, 0);
}

#[test]
fn running_job_ids_reports_only_started_jobs() {
  let budget = TAG_16VCPU_32GB;
  let mut gate = Gate::new(budget, 8);

  let a = JobId::new("job-a"); // started
  let b = JobId::new("job-b"); // admitted, still queued

  assert!(gate.admit(&a, TAG_8VCPU_16GB).is_ok());
  assert_eq!(gate.try_start(&a), Some(StartOutcome::Started));
  assert!(gate.admit(&b, TAG_8VCPU_32GB).is_ok());
  assert_eq!(gate.try_start(&b), Some(StartOutcome::Deferred));

  assert_eq!(gate.running_job_ids(), vec![a.clone()]);

  // Once B starts too, both are reported — order is not asserted, since
  // the gate has no ordering guarantee of its own.
  let freed = gate.release(&a);
  assert_eq!(freed, Some(TAG_8VCPU_16GB));
  assert_eq!(gate.try_start(&b), Some(StartOutcome::Started));
  assert_eq!(gate.running_job_ids(), vec![b]);
}

#[test]
fn a_job_exactly_at_the_total_budget_boundary_is_not_exceeds_budget() {
  // `fits_within` uses `<=`, so a size exactly equal to the budget must be
  // admitted, not rejected — only strictly-larger sizes are `ExceedsBudget`.
  let budget = JobSize {
    vcpu: u32::MAX,
    memory_mb: u32::MAX,
  };
  let mut gate = Gate::new(budget, 1);
  let a = JobId::new("job-a");

  assert_eq!(gate.admit(&a, budget), Ok(()));
}
