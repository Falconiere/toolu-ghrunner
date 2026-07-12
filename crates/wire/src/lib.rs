//! Async I/O for the GitHub Actions JIT protocol: raw HTTP transport
//! ([`net`]) plus the Run/Results Service domain wrappers ([`reporting`]).
//! The two reference each other, so they share a crate to keep the cycle
//! intra-crate. Depends only on [`protocol`] and `shared`.

/// Async network layer: token exchange, session, messages, run service.
pub mod net;
/// Run service / results service domain types and async wrappers.
pub mod reporting;
