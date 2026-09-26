//! Docker client wrapper (bollard).

mod action_archive;
/// Owned Docker action container lifecycle.
pub(crate) mod action_container;
mod action_image;
/// Standard Docker action mounts and path translation.
pub(crate) mod action_mounts;
/// The bollard Docker daemon client wrapper.
pub mod client;
mod container_command;
mod container_create_options;
/// Attached container execution and bounded cancellation.
pub mod container_exec;
mod container_health_options;
mod container_mounts;
/// Validated Docker create options.
pub mod container_options;
mod container_ports;
/// Evaluated job container declarations.
pub mod container_spec;
/// Per-job container and network ownership.
pub mod job_container;
/// Host-to-container path translation for bind mounts.
pub mod path_translator;
/// Service-container lifecycle management (job-level `services:`).
pub mod services;

#[cfg(test)]
#[path = "docker/tests/job_container_failures.rs"]
mod job_container_failures;

/// Service Docker requests and image authentication.
mod service_create;
/// Bounded service health readiness.
mod service_health;
/// Ordered acquired service declarations.
pub(crate) mod service_spec;

#[cfg(test)]
#[path = "docker/tests/action_container.rs"]
mod action_container_tests;

#[cfg(test)]
#[path = "docker/tests/action_container_volumes.rs"]
mod action_container_volume_tests;
