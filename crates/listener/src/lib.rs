//! GitHub Actions listener — full JIT runner protocol lifecycle.

mod execution_loop;
mod handler;
pub mod helpers;
pub(crate) mod job_lifecycle;
pub mod log_uploader;
pub mod loop_decision;
pub mod message_route;
pub mod outage;
pub(crate) mod retry;
mod setup_step;
mod step_reporter;
#[cfg(test)]
#[path = "tests/watchdog_retry.rs"]
mod watchdog_retry;
#[cfg(test)]
#[path = "tests/watchdog_trip.rs"]
mod watchdog_trip;

pub use handler::GitHubListener;
pub(crate) use handler::SessionCtx;
