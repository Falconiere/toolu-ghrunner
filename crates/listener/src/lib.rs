//! GitHub Actions listener — full JIT runner protocol lifecycle.

#[cfg(test)]
#[path = "tests/early_ack.rs"]
mod early_ack;
mod execution_loop;
#[cfg(test)]
#[path = "tests/finalize_split.rs"]
mod finalize_split;
mod handler;
pub mod helpers;
pub(crate) mod job_lifecycle;
pub mod log_uploader;
pub mod loop_decision;
pub mod message_route;
pub mod outage;
pub(crate) mod retry;
mod setup_step;
#[cfg(test)]
#[path = "tests/startup_overlap.rs"]
mod startup_overlap;
mod step_report_queue;
#[cfg(test)]
#[path = "tests/step_report_queue.rs"]
mod step_report_queue_test;
mod step_reporter;
#[cfg(test)]
#[path = "tests/support.rs"]
mod support;
#[cfg(test)]
#[path = "tests/watchdog_retry.rs"]
mod watchdog_retry;
#[cfg(test)]
#[path = "tests/watchdog_trip.rs"]
mod watchdog_trip;

pub use handler::GitHubListener;
pub(crate) use handler::SessionCtx;
