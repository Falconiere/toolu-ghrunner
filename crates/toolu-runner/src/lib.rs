//! GitHub Actions JIT listener, execution engine, and CLI binary.
//!
//! Module map:
//! - [`net`] — async network layer (token exchange, session, job lifecycle).
//! - [`listener`] — GitHub JIT lifecycle (handler, job execution loop).
//! - [`reporting`] — run service, results service, log upload, timeline.
//! - [`execution`] — job execution engine (context, steps runner, handlers).
//! - [`docker`] — bollard wrapper, service containers, path translation.
//! - [`node`] — Node.js runtime detection and caching.
//! - [`plugin`] — `RunnerPlugin` trait and registry.
//! - [`types`] — `RunnerConfig`, `RunnerError`, `RunnerEvent`, message types.
//!
//! Populated progressively in steps 2–9 per the plan.

#![doc(html_root_url = "https://docs.rs/toolu-runner/0.1.0")]

pub mod docker;
pub mod execution;
pub mod listener;
pub mod net;
pub mod node;
pub mod plugin;
pub mod reporting;
pub mod types;
