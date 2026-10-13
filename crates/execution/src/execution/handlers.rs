//! Step handlers: the concrete executors for `run:` scripts and the action
//! kinds a manifest's `runs.using` selects. Selection itself is not here: the
//! step loop (`steps_runner`) splits `run:` from `uses:`, and `action_exec`
//! matches the resolved manifest's `runs.using`.
//!
//! ## Module map
//!
//! - [`script`] — Built-in `run:` shell handler.
//! - [`node`] / [`node_exec`] — Built-in Node.js action handler.
//! - [`composite`] — Built-in composite action handler.
//! - [`docker`] — Built-in Docker action handler.
//!
//! The supported `runs.using` values are `node12`/`node16`/`node20` (run on
//! Node 20), `node24`, `composite` and `docker`. Anything else is rejected
//! when the action manifest is parsed (`actions::manifest`), before a handler
//! is chosen.

/// Built-in composite action handler.
pub mod composite;
/// Built-in Docker action handler.
pub mod docker;
/// Built-in Node.js action handler: env building and input mapping.
pub mod node;
/// Built-in Node.js action handler: runtime resolution and process execution.
pub mod node_exec;
/// Built-in `run:` shell handler.
pub mod script;
/// Shared shell command contract and script preparation.
mod shell_command;
