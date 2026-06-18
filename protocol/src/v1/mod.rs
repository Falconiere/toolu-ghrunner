//! GHES V1 service discovery types and pure URL resolution.
//!
//! The async `V1ServiceDiscovery::discover` lives in `toolu-runner::net`
//! because it hits `/_apis/connectionData` over HTTP. The type shapes and
//! the `resolve_service_url` pure helper stay here.

pub mod discovery;
pub mod types;

pub use discovery::resolve_service_url;
pub use types::{
  api_versions, service_guids, ConnectionData, JobEvent, LocationServiceData, LogReference,
  ServiceDefinition, TimelineRecord, TimelineRecordResult, TimelineRecordState,
};
