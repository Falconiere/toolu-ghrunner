//! GitHub Actions JIT protocol layer — strictly sync, no I/O, no network.
//!
//! This crate owns the protocol types and pure crypto (RSA, JWT, JIT config
//! parsing, AES-CBC decryption). All network calls live in `toolu-runner::net`.
//! The boundary is enforced by the restricted dep set in `Cargo.toml` and
//! verified in CI.

#![doc(html_root_url = "https://docs.rs/toolu-runner-protocol/0.1.0")]

mod jit_config;
mod types;

pub use jit_config::JitConfig;
pub use types::{CredentialData, CredentialDataInner, RsaKeyParams, RunnerSettings};
