//! Local HTTP artifact service mimicking GitHub's artifact API.

mod handlers;
mod lifecycle;

pub use lifecycle::ArtifactService;
