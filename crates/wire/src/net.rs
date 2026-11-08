//! Async transport for the GitHub Actions JIT protocol and Results Service.
//! Pure builders and parsers live in the sync-only `protocol` crate.

/// GitHub App manifest onboarding: loopback callback server + code exchange.
pub mod app_manifest;
/// Broker authentication and token exchange.
pub mod auth;
/// GitHub OAuth device-flow login: request code, poll for token.
pub mod device_auth;
/// GitHub App installation lookup + installation-token minting (#201).
pub mod github_app;
/// Legacy and block-blob log upload requests.
pub mod log_upload;
/// Broker message poll and acknowledgement.
pub mod messages;
/// JIT registration and runner removal.
pub mod register;
/// Results Service transport, including streamed log uploads.
pub mod results_service;
/// Run Service acquire, renew, and completion requests.
pub mod run_service;
/// Organization runner-group listing and name resolution (#201).
pub mod runner_groups;
/// Broker session create and delete requests.
pub mod session;
/// Bounded Results Service step-summary upload.
pub mod step_summary;
/// toolu.sh `/runner/*` job-claim calls of a paired machine (#206).
pub mod toolu_jobs;
/// toolu.sh pairing and enrollment calls of a box becoming a machine (#207).
pub mod toolu_pair;
/// GHES V1 service discovery and timeline transport.
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
pub use messages::{
  PollParams, STATUS_BUSY, STATUS_ONLINE, acknowledge_message, build_poll_url, poll_message,
};
pub use register::{
  JitRegistration, RegisterParams, RunnerScope, build_request, parse_response, register_jit,
  resolve_target, unregister_runner,
};
pub use results_service::{
  create_job_logs_metadata, create_step_logs_metadata, get_job_logs_signed_blob_url,
  get_step_logs_signed_blob_url, update_workflow_steps, upload_log_file,
};
pub use run_service::{acquire_job, complete_job, renew_job};
pub use session::{create_session, delete_session};
pub use v1::{fetch_connection_data, fetch_timeline, post_timeline_record};
