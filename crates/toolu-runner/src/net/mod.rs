//! Async network layer for the GH Actions protocol.
//!
//! This module owns every I/O call: token exchange, session create/delete,
//! acquire/renew/complete job, log upload. The split from the pure
//! `toolu-runner-protocol` crate is enforced by the latter's restricted
//! dep list and verified in CI (AC #22).

// Populated in step 5.
