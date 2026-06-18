//! GitHub Actions JIT protocol layer — strictly sync, no I/O, no network.
//!
//! This crate owns the protocol types and pure crypto (RSA, JWT, JIT config
//! parsing). All network calls live in `toolu-runner::net`. The boundary is
//! enforced by the restricted dep set in `Cargo.toml` and verified in CI
//! (AC #22).

#![doc(html_root_url = "https://docs.rs/toolu-runner-protocol/0.1.0")]
