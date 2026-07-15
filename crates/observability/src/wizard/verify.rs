//! Pure decision for the final "is the runner online?" verify stage: the
//! supervisor service must be active AND the runner's log tail must carry
//! the online marker. No I/O — the bin gathers both facts and passes them.

/// Outcome of the verify stage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyOutcome {
  /// Service active and the online marker was found — confirmed online.
  Online,
  /// Not confirmed online, with a short reason.
  Unconfirmed(String),
}

/// Decide whether the runner is confirmed online: `Online` iff the service
/// is active AND `log_tail` contains `marker`; otherwise `Unconfirmed` with
/// a reason ("service not active" when it is not, else "no online marker in
/// log yet").
pub fn verify_decision(service_active: bool, log_tail: &str, marker: &str) -> VerifyOutcome {
  if service_active && log_tail.contains(marker) {
    return VerifyOutcome::Online;
  }
  let reason = if service_active {
    "no online marker in log yet"
  } else {
    "service not active"
  };
  VerifyOutcome::Unconfirmed(reason.to_owned())
}
