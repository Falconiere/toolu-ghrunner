//! Docker client wrapper (bollard).

/// The bollard Docker daemon client wrapper.
pub mod client;
/// Host-to-container path translation for bind mounts.
pub mod path_translator;
/// Service-container lifecycle management (job-level `services:`).
pub mod services;
