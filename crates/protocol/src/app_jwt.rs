//! GitHub App authentication: the App JWT and the App API reply shapes.
//!
//! A GitHub App proves who it is with a short-lived RS256 JWT signed by its
//! private key (`iss` = the App id). GitHub rejects a JWT whose `exp` lies
//! more than 10 minutes ahead, and recommends backdating `iat` by 60 s for
//! clock drift. The JWT only reaches the App endpoints; everything else uses
//! the installation token it buys. The HTTP calls live in
//! `wire::net::github_app`.

use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde::{Deserialize, Serialize};
use shared::RunnerError;

/// Seconds `iat` is set before now, so a slightly fast GitHub clock still
/// sees the JWT as issued.
pub const APP_JWT_BACKDATE_SECS: i64 = 60;

/// Seconds from now until `exp`. With the backdate, `exp - iat` stays at
/// 10 minutes, the most GitHub accepts.
pub const APP_JWT_LIFETIME_SECS: i64 = 540;

/// The three claims GitHub reads from an App JWT.
#[derive(Debug, Serialize)]
struct AppJwtClaims {
  iat: i64,
  exp: i64,
  iss: String,
}

/// Sign an App JWT for `app_id` with the App's private-key `pem`, valid from
/// now.
///
/// # Errors
///
/// `RunnerError::Config` when `pem` is not a usable RSA private key (the
/// message never repeats the key), `RunnerError::Protocol` on a clock or
/// signing failure.
pub fn build_app_jwt(pem: &str, app_id: i64) -> Result<String, RunnerError> {
  let now = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map_err(|err| RunnerError::Protocol(format!("system time error: {err}")))?;
  let now_secs = i64::try_from(now.as_secs())
    .map_err(|err| RunnerError::Protocol(format!("timestamp overflow: {err}")))?;
  build_app_jwt_at(pem, app_id, now_secs)
}

/// [`build_app_jwt`] at an explicit `now_secs` (Unix seconds): claims
/// `iat = now − 60`, `exp = now + 540`, `iss = "<app_id>"`.
///
/// # Errors
///
/// As [`build_app_jwt`].
pub fn build_app_jwt_at(pem: &str, app_id: i64, now_secs: i64) -> Result<String, RunnerError> {
  // jsonwebtoken's key errors name the failure kind only, never the input.
  let key = EncodingKey::from_rsa_pem(pem.as_bytes()).map_err(|err| {
    RunnerError::Config(format!(
      "the GitHub App private key is not a usable RSA PEM: {err}"
    ))
  })?;
  let claims = AppJwtClaims {
    iat: now_secs.saturating_sub(APP_JWT_BACKDATE_SECS),
    exp: now_secs.saturating_add(APP_JWT_LIFETIME_SECS),
    iss: app_id.to_string(),
  };
  jsonwebtoken::encode(&Header::new(Algorithm::RS256), &claims, &key)
    .map_err(|err| RunnerError::Protocol(format!("App JWT signing failed: {err}")))
}

/// One App installation (`GET /repos/{o}/{r}/installation`,
/// `GET /orgs/{org}/installation`, an item of `GET /app/installations`).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Installation {
  /// The installation id the token endpoint is keyed by.
  pub id: i64,
  /// The user or organization the App is installed on.
  #[serde(default)]
  pub account: Option<InstallationAccount>,
}

/// The account an [`Installation`] belongs to.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct InstallationAccount {
  /// The user or organization login.
  pub login: String,
}

/// `POST /app/installations/{id}/access_tokens` reply: a bearer for the
/// installation's REST calls, valid for one hour.
#[derive(Clone, Deserialize)]
pub struct InstallationToken {
  /// The installation access token (`ghs_…`).
  pub token: String,
  /// When the token expires, RFC 3339; empty when GitHub omitted it.
  #[serde(default)]
  pub expires_at: String,
}

impl std::fmt::Debug for InstallationToken {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("InstallationToken")
      .field("token", &"<redacted>")
      .field("expires_at", &self.expires_at)
      .finish()
  }
}
