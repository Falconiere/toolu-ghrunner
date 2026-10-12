//! Async transport for the JIT auth handshake.
//!
//! The sync crypto (`parse_rsa_private_key`, `build_jwt`) lives in
//! `protocol::auth`. This module owns the single HTTP POST that swaps
//! the signed JWT for an `OAuth2` access token.

use shared::RunnerError;

/// Exchange a signed JWT for an `OAuth2` access token.
///
/// # Errors
///
/// Returns `RunnerError::Network` for transport, 429, or 5xx failures,
/// `RunnerError::Auth` for rejected credentials — 401/403, or an OAuth
/// `invalid_client` error, which GitHub answers (with a 400) once the runner
/// registration was deleted — and `RunnerError::Protocol` for other HTTP or
/// response parse failures.
pub async fn exchange_token(
  client: &reqwest::Client,
  authorization_url: &str,
  jwt: &str,
) -> Result<protocol::AccessToken, RunnerError> {
  let params = [
    (
      "client_assertion_type",
      "urn:ietf:params:oauth:client-assertion-type:jwt-bearer",
    ),
    ("client_assertion", jwt),
    ("grant_type", "client_credentials"),
  ];

  let response = client
    .post(authorization_url)
    .form(&params)
    .send()
    .await
    .map_err(|err| {
      let kind = if err.is_timeout() {
        "timed out"
      } else if err.is_connect() {
        "connection failed"
      } else if err.is_body() {
        "body failed"
      } else {
        "request failed"
      };
      RunnerError::Network(format!("token exchange {kind}"))
    })?;

  let status = response.status();
  if !status.is_success() {
    tracing::debug!(status = %status, "token exchange failed");
    // A 4xx `invalid_client` means the registration is gone. An unreadable
    // body only loses that refinement; the status still classifies below.
    let body = if status.is_client_error() {
      response.text().await.unwrap_or_default()
    } else {
      String::new()
    };
    if oauth_error(&body).as_deref() == Some("invalid_client") {
      return Err(RunnerError::Auth(format!(
        "token exchange failed with status {status}: the runner registration no longer exists \
         on GitHub (OAuth invalid_client)"
      )));
    }
    let message = format!("token exchange failed with status {status}: see debug log");
    return Err(if status.as_u16() == 401 || status.as_u16() == 403 {
      RunnerError::Auth(message)
    } else if status.as_u16() == 429 || status.is_server_error() {
      RunnerError::Network(message)
    } else {
      RunnerError::Protocol(message)
    });
  }

  response
    .json::<protocol::AccessToken>()
    .await
    .map_err(|err| RunnerError::Protocol(format!("token response parse failed: {err}")))
}

/// The OAuth `error` code of a token-endpoint error body, when it is JSON.
fn oauth_error(body: &str) -> Option<String> {
  let value: serde_json::Value = serde_json::from_str(body).ok()?;
  value.get("error")?.as_str().map(str::to_owned)
}

/// Full authentication flow: parse the JIT RSA key, build a JWT, and
/// exchange it for an access token.
///
/// # Errors
///
/// Returns `RunnerError::Protocol` on any step failure (key parse,
/// JWT sign, or HTTP exchange).
pub async fn authenticate(
  client: &reqwest::Client,
  rsa_params: &protocol::RsaKeyParams,
  client_id: &str,
  authorization_url: &str,
) -> Result<protocol::AccessToken, RunnerError> {
  let der_bytes = protocol::parse_rsa_private_key(rsa_params)?;
  let jwt = protocol::build_jwt(&der_bytes, client_id, authorization_url)?;
  exchange_token(client, authorization_url, &jwt).await
}
