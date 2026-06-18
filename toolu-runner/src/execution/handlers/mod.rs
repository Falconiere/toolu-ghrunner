//! Step handler registry and dispatch.
//!
//! ## Step 4c stubs
//!
//! The real implementations of the script/node/node_exec/composite/docker
//! handlers land in step 4d. For step 4c the stubs return `Conclusion::Failure`
//! with a TODO marker so `action_exec` can compile and the build is green.

pub mod composite;
pub mod docker;
pub mod handler_emit;
pub mod node;
pub mod node_exec;
pub mod resolve;
pub mod script;

pub use resolve::{HandlerKind, resolve_handler};
