//! Log upload — step-level and job-level log blob upload to GitHub Results Service.
//!
//! # Public API
//!
//! - [`StreamerConfig`] — configuration for spawning a [`StepLogStreamer`] actor
//! - [`spawn`] — spawn a per-step log streaming actor
//! - [`upload_step_logs`] — upload a spooled step log (3-phase)
//! - [`upload_job_logs`] — upload a spooled combined job log (3-phase)
//! - [`CHANNEL_CAPACITY`] — channel capacity for log line senders

mod streamer;
mod upload;

pub use streamer::{CHANNEL_CAPACITY, StreamerConfig, spawn};
pub use upload::{upload_job_logs, upload_step_logs};
