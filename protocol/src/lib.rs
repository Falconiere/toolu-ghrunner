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

pub use auth::{AccessToken, build_jwt, parse_rsa_private_key};
pub use jit_config::JitConfig;
pub use messages::{
  BrokerMessage, BrokerMigrationBody, MessageType, RunnerJobRequestBody, decrypt_message_body,
  strip_bom, strip_pkcs7_padding,
};
pub use session::{
  AgentInfo, CreateSessionRequest, CreateSessionResponse, EncryptionKey, TaskAgentSession,
  build_session_request,
};
pub use types::{CredentialData, CredentialDataInner, RsaKeyParams, RunnerSettings};
