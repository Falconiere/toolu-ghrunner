//! GitHub OAuth 2.0 device-authorization flow.
//!
//! Powers `toolu-runner login`: [`request_device_code`] starts the flow
//! (the user visits `verification_uri` and enters `user_code`), then
//! [`poll_for_token`] polls until GitHub returns an access token or the
//! attempt terminates. No client secret is used — device flow is designed
//! for public clients.
//!
//! The wire-response classifier [`parse_poll_response`] is pure so it can
//! be unit-tested against real GitHub payloads without any HTTP.

use serde::Deserialize;
use shared::RunnerError;

/// Response to the device-code request: what the user enters, plus poll timing.
#[derive(Debug, Clone, Deserialize)]
pub struct DeviceCodeResponse {
  /// Opaque code the runner presents when polling for the token.
  pub device_code: String,
  /// Short code the user types at `verification_uri`.
  pub user_code: String,
  /// URL the user opens to authorize the device.
  pub verification_uri: String,
  /// Lifetime of `device_code` in seconds.
  pub expires_in: u64,
  /// Minimum seconds to wait between polls.
  pub interval: u64,
}

/// A successfully issued device-flow access token.
#[derive(Debug, Clone, Deserialize)]
pub struct DeviceToken {
  /// The bearer token to persist and reuse.
  pub access_token: String,
  /// Token type GitHub returns (typically `bearer`).
  pub token_type: String,
  /// Space-delimited scopes granted to the token.
  pub scope: String,
}

/// Classification of a single poll response.
#[derive(Debug, Clone)]
pub enum PollOutcome {
  /// `authorization_pending` — keep polling at the current interval.
  Pending,
  /// `slow_down` — keep polling, but add 5s to the interval.
  SlowDown,
  /// The token was issued.
  Token(DeviceToken),
  /// `access_denied` — the user rejected the authorization.
  Denied,
  /// `expired_token` — the device code lapsed before authorization.
  Expired,
  /// Any other terminal error (carries GitHub's `error` string).
  Error(String),
}

/// Start the device flow: `POST https://<host>/login/device/code`.
///
/// Sends `client_id` + `scope` as a form; no client secret.
///
/// # Errors
///
/// Returns `RunnerError::Network` on transport failure and
/// `RunnerError::Auth` on a non-success status or an unparseable body.
pub async fn request_device_code(
  client: &reqwest::Client,
  host: &str,
  client_id: &str,
  scope: &str,
) -> Result<DeviceCodeResponse, RunnerError> {
  let url = format!("https://{host}/login/device/code");
  let response = client
    .post(&url)
    .header("Accept", "application/json")
    .header(
      "User-Agent",
      concat!("toolu-runner/", env!("CARGO_PKG_VERSION")),
    )
    .form(&[("client_id", client_id), ("scope", scope)])
    .send()
    .await
    .map_err(|e| RunnerError::Network(format!("device code request failed: {e}")))?;

  let status = response.status();
  let text = response
    .text()
    .await
    .map_err(|e| RunnerError::Network(format!("reading device code body failed: {e}")))?;

  if !status.is_success() {
    return Err(RunnerError::Auth(format!(
      "device code request returned {status}: {text}"
    )));
  }

  serde_json::from_str(&text)
    .map_err(|e| RunnerError::Auth(format!("parsing device code response failed: {e}")))
}

/// Poll `https://<host>/login/oauth/access_token` until the flow terminates.
///
/// Waits `dc.interval` seconds between polls, bumps the interval by 5s on
/// `slow_down`, loops on `authorization_pending`, and gives up once
/// `dc.expires_in` has elapsed.
///
/// # Errors
///
/// Returns `RunnerError::Network` on transport failure and
/// `RunnerError::Auth` on denial, expiry, or any terminal error.
pub async fn poll_for_token(
  client: &reqwest::Client,
  host: &str,
  client_id: &str,
  dc: &DeviceCodeResponse,
) -> Result<DeviceToken, RunnerError> {
  let url = format!("https://{host}/login/oauth/access_token");
  let start = std::time::Instant::now();
  let deadline = std::time::Duration::from_secs(dc.expires_in);
  let mut interval = dc.interval;

  loop {
    if start.elapsed() >= deadline {
      return Err(RunnerError::Auth("device code expired".to_owned()));
    }
    tokio::time::sleep(std::time::Duration::from_secs(interval)).await;

    let text = send_poll(client, &url, client_id, &dc.device_code).await?;
    match parse_poll_response(&text) {
      PollOutcome::Token(token) => return Ok(token),
      PollOutcome::Pending => {},
      PollOutcome::SlowDown => interval += 5,
      PollOutcome::Denied => {
        return Err(RunnerError::Auth(
          "device authorization denied by user".to_owned(),
        ));
      },
      PollOutcome::Expired => return Err(RunnerError::Auth("device code expired".to_owned())),
      PollOutcome::Error(msg) => {
        return Err(RunnerError::Auth(format!("device flow error: {msg}")));
      },
    }
  }
}

/// POST one token-poll request and return the raw response body.
async fn send_poll(
  client: &reqwest::Client,
  url: &str,
  client_id: &str,
  device_code: &str,
) -> Result<String, RunnerError> {
  let response = client
    .post(url)
    .header("Accept", "application/json")
    .header(
      "User-Agent",
      concat!("toolu-runner/", env!("CARGO_PKG_VERSION")),
    )
    .form(&[
      ("client_id", client_id),
      ("device_code", device_code),
      ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
    ])
    .send()
    .await
    .map_err(|e| RunnerError::Network(format!("device token poll failed: {e}")))?;

  response
    .text()
    .await
    .map_err(|e| RunnerError::Network(format!("reading device token body failed: {e}")))
}

/// Classify a poll-response body into a [`PollOutcome`]. Pure — no I/O.
///
/// A success body carries `access_token`; an error body carries
/// `{"error": "authorization_pending" | "slow_down" | "expired_token" |
/// "access_denied" | …}`. Any unrecognized `error` string becomes
/// [`PollOutcome::Error`].
pub fn parse_poll_response(body: &str) -> PollOutcome {
  if let Ok(token) = serde_json::from_str::<DeviceToken>(body) {
    return PollOutcome::Token(token);
  }

  let parsed: serde_json::Value = match serde_json::from_str(body) {
    Ok(value) => value,
    Err(e) => return PollOutcome::Error(format!("unparseable poll response: {e}")),
  };

  match parsed.get("error").and_then(serde_json::Value::as_str) {
    Some("authorization_pending") => PollOutcome::Pending,
    Some("slow_down") => PollOutcome::SlowDown,
    Some("expired_token") => PollOutcome::Expired,
    Some("access_denied") => PollOutcome::Denied,
    Some(other) => PollOutcome::Error(other.to_owned()),
    None => PollOutcome::Error(format!("poll response missing error field: {body}")),
  }
}
