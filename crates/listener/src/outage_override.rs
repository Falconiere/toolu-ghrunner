//! Folds the mid-job outage watchdog's trip flag into the reported job
//! conclusion and a truthful lock-loss annotation.

use shared::Conclusion;

/// The single source of truth for the outage annotation text — referenced
/// by the `watchdog_trip` assertions so tests cannot drift from the
/// message actually reported to GitHub.
pub(crate) const LOST_CONNECTION_MESSAGE: &str =
  "Runner lost connection to GitHub for more than 5 minutes; job was cancelled (lost connection).";
/// Report immediate renewal 404 without claiming a timed outage.
pub(crate) const JOB_NOT_FOUND_MESSAGE: &str =
  "GitHub no longer recognizes this job; job was cancelled (job lock lost).";

/// Why the renewal task could no longer keep this job's lock.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum RenewalLoss {
  /// No renewal loss occurred.
  None = 0,
  /// GitHub returned 404 for this job.
  JobNotFound = 1,
  /// Renewal failures persisted beyond the outage threshold.
  TimedOut = 2,
}

impl RenewalLoss {
  /// Decode the atomic state written by the renewal task.
  pub(crate) fn from_raw(value: u8) -> Self {
    match value {
      1 => Self::JobNotFound,
      2 => Self::TimedOut,
      _ => Self::None,
    }
  }
}

/// Fold the renewal-loss reason into the engine's conclusion.
///
/// Called after the renewal task is joined, so its reason is final. A lost
/// lock overrides a non-`Success` conclusion to `Failure` with the matching
/// annotation. A `Success` alongside a loss is a teardown race: the job
/// finished before cancellation landed, so its result remains `Success`.
pub(crate) fn apply_outage_override(
  conclusion: Conclusion,
  loss: RenewalLoss,
) -> (Conclusion, Vec<wire::reporting::Annotation>) {
  if loss == RenewalLoss::None {
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
    message: match loss {
      RenewalLoss::JobNotFound => JOB_NOT_FOUND_MESSAGE,
      RenewalLoss::TimedOut => LOST_CONNECTION_MESSAGE,
      RenewalLoss::None => return (conclusion, Vec::new()),
    }
    .to_owned(),
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
