//! Pure URL resolution for GHES V1 service definitions.
//!
//! The async HTTP fetch (`/_apis/connectionData`) lives in
//! `toolu-runner::net::v1_discovery` — only the local lookup stays here.

use super::types::{service_guids, ConnectionData};

/// Resolve a service URL by matching the GUID against service definitions.
pub fn resolve_service_url(base_url: &str, data: &ConnectionData, service_guid: &str) -> Option<String> {
  let service = data
    .location_service_data
    .service_definitions
    .iter()
    .find(|s| s.identifier.eq_ignore_ascii_case(service_guid))?;

  let relative_path = service.relative_path.as_deref()?;
  Some(format!("{base_url}{relative_path}"))
}

/// Resolve the GHES V1 timeline URL.
pub fn timeline_url(base_url: &str, data: &ConnectionData) -> Option<String> {
  resolve_service_url(base_url, data, service_guids::TIMELINE)
}

/// Resolve the GHES V1 log-files upload URL.
pub fn log_files_url(base_url: &str, data: &ConnectionData) -> Option<String> {
  resolve_service_url(base_url, data, service_guids::LOG_FILES)
}

/// Resolve the GHES V1 log-lines append URL.
pub fn log_lines_url(base_url: &str, data: &ConnectionData) -> Option<String> {
  resolve_service_url(base_url, data, service_guids::LOG_LINES)
}

/// Resolve the GHES V1 job-finish URL.
pub fn job_finish_url(base_url: &str, data: &ConnectionData) -> Option<String> {
  resolve_service_url(base_url, data, service_guids::JOB_FINISH)
}

/// Resolve the GHES V1 agent-delete URL.
pub fn agent_delete_url(base_url: &str, data: &ConnectionData) -> Option<String> {
  resolve_service_url(base_url, data, service_guids::AGENT_DELETE)
}
