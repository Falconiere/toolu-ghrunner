//! Built-in GitHub Actions expression functions.

mod builtins;
mod hash;
mod json_convert;

pub use builtins::call_function;
pub use hash::hash_files;
