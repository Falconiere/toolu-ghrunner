//! Cross-cutting types: RunnerConfig (cgroup_path stays as Option<PathBuf> —
//! v1 always has `None` because the JIT listener doesn't enforce cgroup limits).
//!
//! Most types (RunnerError, Conclusion, RunnerEvent, all job_message types) live
//! in the `shared` crate. This module holds runner-specific additions.

mod config;

pub use config::RunnerConfig;
