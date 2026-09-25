//! GitHub Actions listener — full JIT runner protocol lifecycle.

#[cfg(test)]
#[path = "tests/annotation_reporting.rs"]
mod annotation_reporting;
#[cfg(test)]
#[path = "tests/broker_control.rs"]
mod broker_control;
mod broker_message;
mod broker_refresh;
#[cfg(test)]
#[path = "tests/early_ack.rs"]
mod early_ack;
mod execution_loop;
#[cfg(test)]
#[path = "tests/finalize_split.rs"]
mod finalize_split;
mod handler;
/// Shared listener protocol and reporting helpers.
pub mod helpers;
/// Job acquisition, execution, and completion lifecycle.
pub(crate) mod job_lifecycle;
/// Per-step and combined job log upload streams.
pub mod log_uploader;
/// Decision policy for the persistent registration loop.
pub mod loop_decision;
/// Routing decisions for broker messages.
pub mod message_route;
/// Mid-job connection outage detection.
pub mod outage;
#[cfg(test)]
#[path = "tests/post_results.rs"]
mod post_results;
/// Bounded retries for transient Run Service failures.
pub(crate) mod retry;
mod setup_step;
#[cfg(test)]
#[path = "tests/startup_overlap.rs"]
mod startup_overlap;
#[cfg(test)]
#[path = "tests/step_context_identity.rs"]
mod step_context_identity_test;
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
