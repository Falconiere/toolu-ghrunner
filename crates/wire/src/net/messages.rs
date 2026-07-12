//! Async transport for the broker long-poll loop.
//!
//! Crypto for the response body (AES-256-CBC decryption) lives in
//! `protocol::messages`. This module only does the HTTP.

use shared::RunnerError;

/// Bundle of everything a `poll_message` call needs.
///
/// Passed by reference so the listener can keep the underlying
/// `String`s alive across `.await` points without cloning.
pub struct PollParams<'a> {
  pub client: &'a reqwest::Client,
  pub server_url_v2: &'a str,
  pub token: &'a str,
  pub session_id: &'a str,
  pub runner_version: &'a str,
  pub os: &'a str,
  pub architecture: &'a str,
  /// Redelivery cursor: the id of the last message the runner saw. The
  /// broker only returns messages newer than this, so a redelivered or
  /// already-handled message is not re-served. `0` on the first poll.
  pub last_message_id: i64,
}

/// Build the broker long-poll URL, including the `lastMessageId` cursor.
///
/// Extracted so the query contract (notably the redelivery cursor) can be
/// asserted without issuing a request.
pub fn build_poll_url(params: &PollParams<'_>) -> String {
  format!(
    "{}/message?sessionId={}&status=Online\
     &runnerVersion={}&os={}&architecture={}\
     &lastMessageId={}&disableUpdate=true",
    params.server_url_v2,
    params.session_id,
    params.runner_version,
    params.os,
    params.architecture,
    params.last_message_id
  )
}

/// Long-poll the broker for a job assignment.
///
/// Returns:
/// - `Ok(None)` on HTTP 202 (no work, caller should re-poll).
/// - `Ok(Some(message))` on HTTP 200.
///
/// # Errors
///
/// Returns `RunnerError::Protocol` on transport or parse failures, or
/// on any status code other than 200/202.
pub async fn poll_message(
  params: &PollParams<'_>,
) -> Result<Option<protocol::BrokerMessage>, RunnerError> {
  let url = build_poll_url(params);

  let response = params
    .client
    .get(&url)
    .bearer_auth(params.token)
    .send()
    .await
    .map_err(|e| RunnerError::Protocol(format!("message poll failed: {e}")))?;

  let status = response.status().as_u16();
  match status {
    202 => Ok(None),
    200 => response
      .json::<protocol::BrokerMessage>()
      .await
      .map(Some)
      .map_err(|e| RunnerError::Protocol(format!("message parse failed: {e}"))),
    other => {
      let body = response.text().await.unwrap_or_default();
      tracing::debug!(status = other, body = %body, "message poll failed");
      Err(RunnerError::Protocol(format!(
        "message poll returned status {other}: see debug log"
      )))
    },
  }
}

/// Acknowledge a broker message. Must be called before acquiring the job.
///
/// # Errors
///
/// Returns `RunnerError::Protocol` on HTTP failures.
pub async fn acknowledge_message(
  client: &reqwest::Client,
  server_url_v2: &str,
  token: &str,
  runner_request_id: &str,
) -> Result<(), RunnerError> {
  let base = server_url_v2.trim_end_matches('/');
  // The broker validates the acknowledge request: it needs the runner
  // `status` (empty → 400 "invalid runner status") and the `runnerRequestId`
  // of the job request (missing → 400 "Missing runnerRequestId"), plus the
  // standard runner metadata — matching the poll's `?status=Online` contract
  // (C# AcknowledgeRunnerRequestAsync sends runnerRequestId/status/os/arch/version).
  //
  // os/arch are derived from the same `context_build` helpers the poll path
  // uses, so a single runner advertises one consistent os/arch on both calls.
  let url = format!("{base}/acknowledge");
  let response = client
    .post(&url)
    .query(&[
      ("runnerRequestId", runner_request_id),
      ("status", "Online"),
      ("os", shared::platform::runner_os()),
      ("architecture", shared::platform::runner_arch()),
      ("runnerVersion", env!("CARGO_PKG_VERSION")),
    ])
    .bearer_auth(token)
    .json(&serde_json::json!({ "runnerRequestId": runner_request_id }))
    .send()
    .await
    .map_err(|e| RunnerError::Protocol(format!("acknowledge failed: {e}")))?;

  let status = response.status();
  if !status.is_success() {
    let body = response.text().await.unwrap_or_default();
    tracing::debug!(status = %status, body = %body, "acknowledge failed");
    return Err(RunnerError::Protocol(format!(
      "acknowledge status {status}: see debug log"
    )));
  }

  Ok(())
}
