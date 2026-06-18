//! Async transport for broker session lifecycle.

use shared::RunnerError;

/// Create a broker session.
///
/// # Errors
///
/// Returns `RunnerError::Protocol` on HTTP or response parse failures.
pub async fn create_session(
  client: &reqwest::Client,
  server_url: &str,
  token: &str,
  request: &protocol::CreateSessionRequest,
) -> Result<protocol::CreateSessionResponse, RunnerError> {
  let base = server_url.trim_end_matches('/');
  let url = format!("{base}/session");

  let response = client
    .post(&url)
    .bearer_auth(token)
    .json(request)
    .send()
    .await
    .map_err(|e| RunnerError::Protocol(format!("create session request failed: {e}")))?;

  let status = response.status();
  if !status.is_success() {
    let body = response.text().await.unwrap_or_default();
    tracing::debug!(status = %status, body = %body, "create session failed");
    return Err(RunnerError::Protocol(format!(
      "create session failed with status {status}: see debug log"
    )));
  }

  response
    .json::<protocol::CreateSessionResponse>()
    .await
    .map_err(|e| RunnerError::Protocol(format!("session response parse failed: {e}")))
}

/// Delete a broker session (cleanup on exit).
///
/// Status codes other than 2xx are logged but treated as success —
/// the broker may already have forgotten about JIT runners.
///
/// # Errors
///
/// Returns `RunnerError::Protocol` on transport-level failures.
pub async fn delete_session(
  client: &reqwest::Client,
  server_url: &str,
  token: &str,
  session_id: &str,
) -> Result<(), RunnerError> {
  let base = server_url.trim_end_matches('/');
  let url = format!("{base}/session/{session_id}");

  let response = client
    .delete(&url)
    .bearer_auth(token)
    .send()
    .await
    .map_err(|e| RunnerError::Protocol(format!("delete session request failed: {e}")))?;

  let status = response.status();
  if !status.is_success() {
    tracing::debug!("session delete returned status {status} (expected for JIT runners)");
  }

  Ok(())
}
