//! Cross-cutting types and startup utilities for toolu-runner.
//!
//! This crate is the smallest of the three: types, error enum, and the
//! tracing init. No async, no I/O beyond local file paths in the startup
//! module.

#![doc(html_root_url = "https://docs.rs/toolu-runner-shared/0.1.0")]
