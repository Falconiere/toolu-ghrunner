//! Async network layer for the GitHub Actions JIT protocol.
//!
//! This module owns every I/O call the runner makes: token exchange,
//! session lifecycle, broker long-poll, job acquire/renew/complete,
//! log upload, results service Twirp RPCs, and V1 service discovery.
//!
//! The boundary with [`protocol`] is one-way: every `pub async fn` here
//! takes a [`reqwest::Client`] plus request types from `protocol`, and
//! returns either `protocol` response types or `shared::RunnerError`.
//! `protocol` itself is sync-only and never depends on this crate —
//! that invariant is enforced by `protocol/Cargo.toml`'s dep set.
//!
//! The split exists so we can unit-test the JWT/RSA/AES crypto in
//! `protocol` without spinning up an HTTP client, and so the listener
//! can compose [`crate::reporting`] domain logic on top of these thin
//! transport wrappers.

/// GitHub App manifest onboarding: loopback callback server + code exchange.
pub mod app_manifest;
pub mod auth;
/// GitHub OAuth device-flow login: request code, poll for token.
pub mod device_auth;
pub mod log_upload;
pub mod messages;
pub mod register;
pub mod results_service;
pub mod run_service;
pub mod session;
pub mod v1;

pub use app_manifest::{CallbackServer, convert_manifest_code};
pub use auth::{authenticate, exchange_token};
pub use device_auth::{
  DeviceCodeResponse, DeviceToken, PollOutcome, parse_poll_response, poll_for_token,
  request_device_code,
};
pub use log_upload::{
  append_block_headers, block_blob_headers, create_append_blob_headers, upload_block_blob,
  upload_log,
};
pub use messages::{PollParams, acknowledge_message, build_poll_url, poll_message};
pub use register::{JitRegistration, RegisterParams, build_request, parse_response, register_jit};
pub use results_service::{
  create_job_logs_metadata, create_step_logs_metadata, get_job_logs_signed_blob_url,
  get_step_logs_signed_blob_url, update_workflow_steps, upload_log_blob,
};
pub use run_service::{acquire_job, complete_job, renew_job};
pub use session::{create_session, delete_session};
pub use v1::{fetch_connection_data, fetch_timeline, post_timeline_record};
