//! Folds the mid-job outage watchdog's trip flag into the reported job
//! conclusion and its "lost connection" annotation.

use shared::Conclusion;

/// The single source of truth for the outage annotation text — referenced
/// by the `watchdog_trip` assertions so tests cannot drift from the
/// message actually reported to GitHub.
pub(crate) const LOST_CONNECTION_MESSAGE: &str =
  "Runner lost connection to GitHub for more than 5 minutes; job was cancelled (lost connection).";

/// Fold the outage watchdog's trip flag into the engine's conclusion.
///
/// Called only after the renewal task's `JoinHandle` has been awaited, so
/// there is no race between "the watchdog is still writing the flag" and
/// "we are reading it". A tripped flag overrides a non-`Success`
/// conclusion to `Failure` plus the "lost connection" annotation (an
/// honest verdict either way: a genuinely-failed step, or a GH-initiated
/// cancel racing the trip, both happened during a real outage). A tripped
/// flag alongside a `Success` conclusion can only be a
/// trip-during-teardown race — the job finished before the cancel
/// landed — so it is left as `Success`, WARN-logged once, with no
/// annotation; rewriting a successful job's history would be dishonest.
pub(crate) fn apply_outage_override(
  conclusion: Conclusion,
  outage_tripped: bool,
) -> (Conclusion, Vec<wire::reporting::Annotation>) {
  if !outage_tripped {
    return (conclusion, Vec::new());
  }
  if conclusion == Conclusion::Success {
    tracing::warn!(
      "outage watchdog tripped after the job already completed successfully \
       (trip-during-teardown race) — leaving conclusion as Success"
    );
    return (conclusion, Vec::new());
  }
  let annotation = wire::reporting::Annotation {
    level: wire::reporting::ReportAnnotationLevel::Failure,
    message: LOST_CONNECTION_MESSAGE.to_owned(),
    title: None,
    raw_details: None,
    path: None,
    is_infrastructure_issue: false,
    start_line: 0,
    end_line: 0,
    start_column: 0,
    end_column: 0,
    step_number: 0,
  };
  (Conclusion::Failure, vec![annotation])
}
