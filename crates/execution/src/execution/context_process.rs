//! Per-job process tracking accessors (issue #89).

use super::ExecutionContext;
use crate::execution::orphan_cleanup::{ProcessTracking, TRACKING_ENV};

impl ExecutionContext {
  /// Install this job's tracking id (`None` when `process.clean=false`).
  pub(crate) fn set_process_tracking(&mut self, tracking: Option<ProcessTracking>) {
    self.process_tracking = tracking;
  }

  /// This job's tracking id, swept at job end when present.
  pub(crate) fn process_tracking(&self) -> Option<&ProcessTracking> {
    self.process_tracking.as_ref()
  }

  /// The `RUNNER_TRACKING_ID` a host step process starts with: this job's id,
  /// or — when `process.clean=false`, where upstream sets nothing — the value
  /// the runner itself inherited. Workflow env layers still override it.
  pub(crate) fn process_tracking_env(&self) -> Option<String> {
    self.process_tracking.as_ref().map_or_else(
      || crate::config::var(TRACKING_ENV),
      |tracking| Some(tracking.id().to_owned()),
    )
  }
}
