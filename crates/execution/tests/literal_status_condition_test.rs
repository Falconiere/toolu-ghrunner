//! T2b: `ExecutionContext::literal_status_condition` — the `evaluate_condition`
//! fast path for the four zero-argument status functions.
//!
//! Covers all four functions (`success()`, `always()`, `failure()`,
//! `cancelled()`) across every `JobStatus` variant, plus fall-through forms
//! that MUST still reach the full evaluator (`!cancelled()`,
//! `success() && …`, `${{ success() }}`, different casing, empty string).
//! Semantics are pinned against
//! `expressions::functions::builtins::call_function`: `success`/`failure`/
//! `cancelled` compare `job_status`; `always` is unconditionally `true`.

use execution::execution::context::ExecutionContext;
use expressions::evaluator::JobStatus;

const ALL_STATUSES: [JobStatus; 3] = [JobStatus::Success, JobStatus::Failure, JobStatus::Cancelled];

/// Compile-time exhaustiveness guard for [`ALL_STATUSES`]: this `match` has
/// no wildcard arm, so a new `JobStatus` variant fails the build here — a
/// forcing function to update `ALL_STATUSES` (and the fast-path tests above)
/// rather than silently under-covering it.
#[test]
fn all_statuses_matches_every_job_status_variant() {
  for status in ALL_STATUSES {
    match status {
      JobStatus::Success | JobStatus::Failure | JobStatus::Cancelled => {},
    }
  }
}

fn ctx_with_status(status: JobStatus) -> ExecutionContext {
  let mut ctx = ExecutionContext::new_for_test();
  ctx.set_job_status_for_test(status);
  ctx
}

#[test]
fn success_answers_from_job_status_across_all_statuses() {
  for status in ALL_STATUSES {
    let ctx = ctx_with_status(status);
    assert_eq!(
      ctx.literal_status_condition("success()"),
      Some(status == JobStatus::Success),
      "status = {status:?}"
    );
  }
}

#[test]
fn failure_answers_from_job_status_across_all_statuses() {
  for status in ALL_STATUSES {
    let ctx = ctx_with_status(status);
    assert_eq!(
      ctx.literal_status_condition("failure()"),
      Some(status == JobStatus::Failure),
      "status = {status:?}"
    );
  }
}

#[test]
fn cancelled_answers_from_job_status_across_all_statuses() {
  for status in ALL_STATUSES {
    let ctx = ctx_with_status(status);
    assert_eq!(
      ctx.literal_status_condition("cancelled()"),
      Some(status == JobStatus::Cancelled),
      "status = {status:?}"
    );
  }
}

#[test]
fn always_is_unconditionally_true_across_all_statuses() {
  for status in ALL_STATUSES {
    let ctx = ctx_with_status(status);
    assert_eq!(
      ctx.literal_status_condition("always()"),
      Some(true),
      "status = {status:?}"
    );
  }
}

#[test]
fn surrounding_whitespace_is_trimmed() {
  let ctx = ctx_with_status(JobStatus::Success);
  assert_eq!(ctx.literal_status_condition("  success()  "), Some(true));
}

#[test]
fn negated_and_compound_conditions_fall_through() {
  // job_status is Cancelled, so a correct fall-through evaluator would flip
  // the naive fast-path answer for `!cancelled()` — proving these MUST NOT
  // be matched here.
  let ctx = ctx_with_status(JobStatus::Cancelled);
  assert_eq!(ctx.literal_status_condition("!cancelled()"), None);
  assert_eq!(
    ctx.literal_status_condition("success() && vars.x == '1'"),
    None
  );
  assert_eq!(ctx.literal_status_condition("${{ success() }}"), None);
  assert_eq!(ctx.literal_status_condition("Success()"), None);
  assert_eq!(ctx.literal_status_condition(""), None);
}
