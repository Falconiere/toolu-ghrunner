//! Step handler registry and dispatch.
//!
//! ## Module map
//!
//! - [`resolve`] — Pick which handler runs a step based on `runs.using`.
//! - [`script`] — Built-in `run:` shell handler.
//! - [`node`] / [`node_exec`] — Built-in Node.js action handler.
//! - [`composite`] — Built-in composite action handler.
//! - [`docker`] — Built-in Docker action handler.
//!
//! ## Yamless cuts
//!
//! `yamless.rs`, `yamless_deploy/`, `yamless_notify.rs`, and
//! `yamless_test_report/` from the upstream yamless-runner are intentionally
//! dropped — the `HandlerKind::Yamless` variant and the `runs_using ==
//! "yamless"` dispatch are also removed. `handler_emit.rs` was dropped along
//! with its only consumers.

pub mod composite;
pub mod docker;
pub mod node;
pub mod node_exec;
pub mod resolve;
pub mod script;

pub use resolve::{HandlerKind, resolve_handler};
