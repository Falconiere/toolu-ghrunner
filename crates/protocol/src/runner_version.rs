//! GitHub protocol compatibility identity, independent of toolu's release version.

/// Official runner baseline advertised in session, poll and acknowledgement.
///
/// Release v2.338.2 is `d1eb12d4b115c301201de27d667284e1a80f2ab5`.
/// Delta from v2.337.0 reviewed in `docs/runner-updates.md`. Bump only with compatibility revalidation; see
/// `docs/runner-updates.md`. This does not guarantee GitHub queue eligibility.
pub const COMPATIBILITY_VERSION: &str = "2.338.2";
