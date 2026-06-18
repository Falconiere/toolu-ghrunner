//! GitHub Actions JIT protocol layer — strictly sync, no I/O, no network.
//!
//! This crate owns the protocol types and pure crypto (RSA, JWT, JIT config
//! parsing, AES-CBC decryption). All network calls live in `toolu-runner::net`.
//! The boundary is enforced by the restricted dep set in `Cargo.toml` and
//! verified in CI.

#![doc(html_root_url = "https://docs.rs/toolu-runner-protocol/0.1.0")]

pub mod auth;
mod jit_config;
pub mod messages;
pub mod session;
mod types;
pub mod v1;

pub use auth::{build_jwt, parse_rsa_private_key, AccessToken};
pub use jit_config::JitConfig;
pub use messages::{
  decrypt_message_body, strip_bom, strip_pkcs7_padding, BrokerMessage, BrokerMigrationBody,
  MessageType, RunnerJobRequestBody,
};
pub use session::{
  build_session_request, AgentInfo, CreateSessionRequest, CreateSessionResponse, EncryptionKey,
  TaskAgentSession,
};
pub use types::{CredentialData, CredentialDataInner, RsaKeyParams, RunnerSettings};
