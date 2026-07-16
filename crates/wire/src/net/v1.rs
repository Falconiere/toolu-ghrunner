//! Async transport for GHES V1 service discovery and timeline reporting.
//!
//! The pure URL resolvers (`timeline_url`, `log_files_url`, …) live in
//! `protocol::v1`. This module owns the HTTP fetches those resolvers
//! are typically used with.

use shared::RunnerError;

/// Fetch `/_apis/connectionData` from a GHES instance.
///
/// The returned [`protocol::v1::ConnectionData`] is then fed to the pure
/// `protocol::v1::*_url` resolvers to obtain per-service endpoints.
///
/// # Errors
///
/// Returns `RunnerError::Protocol` on transport or parse failures.
pub async fn fetch_connection_data(
  client: &reqwest::Client,
  base_url: &str,
  token: &str,
) -> Result<protocol::v1::ConnectionData, RunnerError> {
  let base = base_url.trim_end_matches('/');
  let url = format!("{base}/_apis/connectionData");

  let response = client
    .get(&url)
    .bearer_auth(token)
    .send()
    .await
    .map_err(|e| RunnerError::Protocol(format!("V1 discovery request failed: {e}")))?;

  if !response.status().is_success() {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    tracing::debug!(status = %status, body_len = body.len(), "V1 discovery failed");
    return Err(RunnerError::Protocol(format!(
      "V1 discovery returned {status}: see debug log"
    )));
  }

  response
    .json::<protocol::v1::ConnectionData>()
    .await
    .map_err(|e| RunnerError::Protocol(format!("V1 discovery parse failed: {e}")))
}

/// Fetch a single timeline record by id from a GHES V1 timeline URL.
///
/// The returned JSON is intentionally untyped — the listener can pick
/// out the fields it cares about without us re-declaring the full
/// V1 timeline schema here.
///
/// # Errors
///
/// Returns `RunnerError::Protocol` on transport or parse failures.
pub async fn fetch_timeline(
  client: &reqwest::Client,
  timeline_url: &str,
  token: &str,
  record_id: &str,
) -> Result<serde_json::Value, RunnerError> {
  let url = format!(
    "{}/{}?api-version={}",
    timeline_url,
    record_id,
    protocol::v1::api_versions::DEFAULT
  );

  let response = client
    .get(&url)
    .bearer_auth(token)
    .send()
    .await
    .map_err(|e| RunnerError::Protocol(format!("timeline fetch failed: {e}")))?;

  let status = response.status();
  if !status.is_success() {
    let body = response.text().await.unwrap_or_default();
    tracing::debug!(status = %status, body_len = body.len(), "timeline fetch failed");
    return Err(RunnerError::Protocol(format!(
      "timeline fetch status {status}: see debug log"
    )));
  }

  response
    .json::<serde_json::Value>()
    .await
    .map_err(|e| RunnerError::Protocol(format!("timeline parse failed: {e}")))
}

/// Post a single timeline record update to a GHES V1 timeline URL.
///
/// # Errors
///
/// Returns `RunnerError::Protocol` on transport failures.
pub async fn post_timeline_record(
  client: &reqwest::Client,
  timeline_url: &str,
  token: &str,
  record: &protocol::v1::TimelineRecord,
) -> Result<(), RunnerError> {
  let base = timeline_url.trim_end_matches('/');
  let url = format!("{base}?api-version={}", protocol::v1::api_versions::DEFAULT);

  let response = client
    .post(&url)
    .bearer_auth(token)
    .json(record)
    .send()
    .await
    .map_err(|e| RunnerError::Protocol(format!("timeline post failed: {e}")))?;

  let status = response.status();
  if !status.is_success() {
    let body = response.text().await.unwrap_or_default();
    tracing::debug!(status = %status, body_len = body.len(), "timeline post failed");
    return Err(RunnerError::Protocol(format!(
      "timeline post status {status}: see debug log"
    )));
  }

  Ok(())
}
