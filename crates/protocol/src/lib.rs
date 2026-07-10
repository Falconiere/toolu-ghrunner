//! GitHub Actions JIT protocol layer — strictly sync, no I/O, no network.
//!
//! This crate owns the protocol types and pure crypto (RSA, JWT, JIT config
//! parsing, AES-CBC decryption). All network calls live in `toolu-runner::net`.
//! The boundary is enforced by the restricted dep set in `Cargo.toml` and
//! verified in CI.

#![doc(html_root_url = "https://docs.rs/toolu-runner-protocol/0.1.0")]

/// JIT auth crypto: RSA key reconstruction and PS256 JWT signing.
pub mod auth;
/// End-to-end decrypt of an encrypted broker message body.
pub mod body_decrypt;
mod jit_config;
/// Broker message shapes and the AES-256-CBC body codec.
pub mod messages;
/// RSA-OAEP unwrap of the session AES key.
pub mod rsa_oaep;
/// Session lifecycle request/response shapes and the encryption key.
pub mod session;
mod types;
/// GHES V1 protocol types and the pure service-URL resolver.
pub mod v1;

pub use auth::{AccessToken, build_jwt, parse_rsa_private_key};
pub use body_decrypt::decrypt_broker_body;
pub use jit_config::JitConfig;
pub use messages::{
  BrokerMessage, BrokerMigrationBody, JobCancelBody, MessageType, RunnerJobRequestBody,
  decrypt_message_body, strip_bom, strip_pkcs7_padding,
};
pub use rsa_oaep::unwrap_aes_key_rsa_oaep;
pub use session::{
  AgentInfo, CreateSessionRequest, CreateSessionResponse, EncryptionKey, TaskAgentSession,
  build_session_request,
};
pub use types::{CredentialData, CredentialDataInner, RsaKeyParams, RunnerSettings};
