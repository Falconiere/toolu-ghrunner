//! Pure conclusion-fold tests for renewal outage reporting.

use shared::Conclusion;

use crate::outage_override::{LOST_CONNECTION_MESSAGE, apply_outage_override};

/// (a) An untripped flag leaves the conclusion unchanged and adds no
/// annotations, whatever the conclusion was.
#[test]
fn untripped_leaves_conclusion_and_annotations_unchanged() {
  for conclusion in [
    Conclusion::Success,
    Conclusion::Failure,
    Conclusion::Cancelled,
    Conclusion::Skipped,
  ] {
    let (out, annotations) = apply_outage_override(conclusion, false);
    assert_eq!(out, conclusion, "untripped must not change the conclusion");
    assert!(annotations.is_empty(), "untripped must add no annotations");
  }
}

/// (b) A tripped flag alongside a `Success` conclusion is the
/// trip-during-teardown race (the job finished before the cancel
/// landed) — it stays `Success`, with no annotation.
#[test]
fn tripped_success_stays_success_with_no_annotation() {
  let (conclusion, annotations) = apply_outage_override(Conclusion::Success, true);
  assert_eq!(conclusion, Conclusion::Success);
  assert!(annotations.is_empty());
}

/// (c) A tripped flag alongside a non-`Success` conclusion (e.g. a
/// GH-initiated `Cancelled` racing the trip) overrides to `Failure` with
/// exactly one failure-level annotation carrying the exact
/// production message.
#[test]
fn tripped_non_success_overrides_to_failure_with_error_annotation() {
  for original in [
    Conclusion::Failure,
    Conclusion::Cancelled,
    Conclusion::Skipped,
  ] {
    let (conclusion, annotations) = apply_outage_override(original, true);
    assert_eq!(conclusion, Conclusion::Failure);
    assert_eq!(annotations.len(), 1);
    let annotation = annotations.first().expect("checked len() == 1 above");
    assert_eq!(
      annotation.level,
      wire::reporting::ReportAnnotationLevel::Failure
    );
    assert_eq!(annotation.message, LOST_CONNECTION_MESSAGE);
  }
}
