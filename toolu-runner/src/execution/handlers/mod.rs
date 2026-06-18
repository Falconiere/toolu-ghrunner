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

pub mod composite;
pub mod docker;
pub mod node;
pub mod node_exec;
pub mod resolve;
pub mod script;

pub use resolve::{HandlerKind, resolve_handler};
