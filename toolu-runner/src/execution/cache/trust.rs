//! Write-side trust for the branch-scoped cache index.
//!
//! Read isolation is the scope ladder (`scope.rs`); CAS chunks are
//! content-verified and shared freely. This gates only writes to a
//! protected scope: allowed only when [`classify_trust`] is
//! [`TrustLevel::Trusted`] — a trusting event *on* a protected branch.
//! Every other case is untrusted and writes only its own scope.

/// Trust level assigned to a job's cache writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustLevel {
  /// A trusting event running on a protected branch — may write a
  /// protected/default index scope.
  Trusted,
  /// Anything else — may write only its own non-protected scope.
  Untrusted,
}

/// Classify a job's write trust from its trigger event and branch.
///
/// Only a trusting event *on a protected branch* is [`TrustLevel::Trusted`].
/// Any event on a non-protected branch, and every non-trusting event
/// (`pull_request`, `pull_request_target`, `workflow_run`, `issue_comment`,
/// anything unrecognized), is [`TrustLevel::Untrusted`].
pub fn classify_trust(
  trigger_event: &str,
  branch: &str,
  protected_branches: &[String],
) -> TrustLevel {
  match trigger_event {
    "push" | "schedule" | "workflow_dispatch" | "release" | "merge_group"
      if is_protected(branch, protected_branches) =>
    {
      TrustLevel::Trusted
    },
    _ => TrustLevel::Untrusted,
  }
}

/// Whether `branch` is in the configured protected/default set.
pub fn is_protected(branch: &str, protected: &[String]) -> bool {
  protected.iter().any(|p| p == branch)
}

/// Whether a job of `trust` may write index `scope` given the `protected` set.
///
/// A protected scope is writable only by a [`TrustLevel::Trusted`] job; a
/// protected write from an [`TrustLevel::Untrusted`] job is refused. Every
/// non-protected scope is always writable. Shared by the v2 Twirp and v1 REST
/// write paths so both gate identically.
pub fn write_allowed(scope: &str, trust: TrustLevel, protected: &[String]) -> bool {
  !(is_protected(scope, protected) && trust == TrustLevel::Untrusted)
}
