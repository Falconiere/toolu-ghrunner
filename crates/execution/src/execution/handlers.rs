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
//! Only the four built-in `runs.using` values from upstream GitHub Actions
//! are dispatched (`script`, `node20`, `composite`, `docker`). Any unknown
//! `runs.using` fails the step with a clear "unsupported runner type" error.

/// Built-in composite action handler.
pub mod composite;
/// Built-in Docker action handler.
pub mod docker;
/// Built-in Node.js action handler: env building and input mapping.
pub mod node;
/// Built-in Node.js action handler: runtime resolution and process execution.
pub mod node_exec;
/// Picks which handler runs a step based on `runs.using`.
pub mod resolve;
/// Built-in `run:` shell handler.
pub mod script;

pub use resolve::{HandlerKind, resolve_handler};
