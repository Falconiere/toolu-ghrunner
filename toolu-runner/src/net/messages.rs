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
  let url = format!(
    "{}/message?sessionId={}&status=Online\
     &runnerVersion={}&os={}&architecture={}\
     &disableUpdate=true",
    params.server_url_v2, params.session_id, params.runner_version, params.os, params.architecture
  );

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
      Err(RunnerError::Protocol(format!(
        "message poll returned status {other}: {body}"
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
  message_id: i64,
) -> Result<(), RunnerError> {
  let base = server_url_v2.trim_end_matches('/');
  let url = format!("{base}/acknowledge");

  let response = client
    .post(&url)
    .bearer_auth(token)
    .json(&serde_json::json!({ "messageId": message_id }))
    .send()
    .await
    .map_err(|e| RunnerError::Protocol(format!("acknowledge failed: {e}")))?;

  let status = response.status();
  if !status.is_success() {
    let body = response.text().await.unwrap_or_default();
    return Err(RunnerError::Protocol(format!(
      "acknowledge status {status}: {body}"
    )));
  }

  Ok(())
}
