//! Cross-cutting types and startup utilities for toolu-runner.
//!
//! This crate is the smallest of the three: types, error enum, and the
//! tracing init. No async, no I/O beyond local file paths in the startup
//! module.

#![doc(html_root_url = "https://docs.rs/shared/0.1.0")]

mod config;
mod error;
mod events;

pub use config::RunnerConfig;
pub use error::RunnerError;
pub use events::{AnnotationLevel, Conclusion, ListenerEvent, LogStream, RunnerEvent};
