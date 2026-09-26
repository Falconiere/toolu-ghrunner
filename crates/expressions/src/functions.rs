//! Built-in GitHub Actions expression functions.

mod builtins;
/// Lazy format placeholder rendering.
pub(crate) mod format;
mod glob_walk;
mod hash;
mod json_convert;

pub use builtins::call_function;
pub use hash::hash_files;
