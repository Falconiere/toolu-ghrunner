//! GitHub protocol compatibility identity, independent of toolu's release version.

/// Official runner baseline advertised in session, poll and acknowledgement.
///
/// Release v2.337.0 is `397b032cbf865e9c3ddfab89d533ec19325e1273`.
/// The epic's later source snapshot `cab9d1c` retains that version in
/// `src/runnerversion`. Bump only with compatibility revalidation; see
/// `docs/runner-updates.md`. This does not guarantee GitHub queue eligibility.
pub const COMPATIBILITY_VERSION: &str = "2.337.0";
