//! Trust-level cache isolation.
//!
//! Each cache write is tagged with a trust level based on the triggering event.
//! PR/fork workflows are untrusted; push to protected branches is trusted.
//! Untrusted jobs can read trusted cache but write only to isolated PR scope.

/// Trust level assigned to cache writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustLevel {
  /// Push to protected/default branch — can write to shared cache.
  Trusted,
  /// PR, fork, or unrecognized trigger — writes to isolated namespace.
  Untrusted,
}

/// Classify a job's trust level based on trigger event and branch.
pub fn classify_trust(
  trigger_event: &str,
  branch: &str,
  protected_branches: &[String],
) -> TrustLevel {
  match trigger_event {
    "push" if is_protected(branch, protected_branches) => TrustLevel::Trusted,
    "schedule" | "workflow_dispatch" => TrustLevel::Trusted,
    _ => TrustLevel::Untrusted,
  }
}

/// Build the cache namespace path for a given trust level.
///
/// Trusted: `{repo}/shared/{key}`
/// Untrusted: `{repo}/pr/{branch}/{key}`
pub fn cache_namespace(trust: TrustLevel, repo: &str, branch: &str, key: &str) -> String {
  match trust {
    TrustLevel::Trusted => format!("{repo}/shared/{key}"),
    TrustLevel::Untrusted => format!("{repo}/pr/{branch}/{key}"),
  }
}

fn is_protected(branch: &str, protected: &[String]) -> bool {
  protected.iter().any(|p| p == branch)
}
