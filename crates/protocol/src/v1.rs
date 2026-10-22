//! GHES V1 service discovery types and pure URL resolution.
//!
//! The async `fetch_connection_data` lives in `wire::net::v1` because it
//! hits `/_apis/connectionData` over HTTP. The type shapes and the
//! `resolve_service_url` pure helper stay here.

pub mod discovery;
pub mod types;

pub use discovery::resolve_service_url;
pub use types::{
  ConnectionData, JobEvent, LocationServiceData, LogReference, ServiceDefinition, TimelineRecord,
  TimelineRecordResult, TimelineRecordState, api_versions, service_guids,
};
