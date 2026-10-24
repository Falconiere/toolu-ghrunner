//! GitHub App authentication: the App JWT and the App API reply shapes
//! (installations, tokens, `GET /app`, `GET /orgs/{org}`).
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
/// signing failure, or when `iat` / `exp` would overflow `i64`.
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
  let out_of_range = || RunnerError::Protocol(format!("App JWT time {now_secs} is out of range"));
  let claims = AppJwtClaims {
    iat: now_secs
      .checked_sub(APP_JWT_BACKDATE_SECS)
      .ok_or_else(out_of_range)?,
    exp: now_secs
      .checked_add(APP_JWT_LIFETIME_SECS)
      .ok_or_else(out_of_range)?,
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
  /// `"all"` or `"selected"` repositories; `None` when GitHub omits it.
  #[serde(default)]
  pub repository_selection: Option<String>,
}

/// The account an [`Installation`] belongs to.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct InstallationAccount {
  /// The user or organization login.
  pub login: String,
}

/// `GET /app` reply: the App the JWT authenticates as (#202 `--import`
/// proves its key with it and refreshes the saved identity from it).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct AppInfo {
  /// Numeric App id.
  pub id: i64,
  /// URL slug.
  pub slug: String,
  /// Display name.
  pub name: String,
  /// The account that owns the App.
  #[serde(default)]
  pub owner: Option<AppOwner>,
  /// The App's public page.
  pub html_url: String,
  /// OAuth client id; empty when GitHub omits it.
  #[serde(default)]
  pub client_id: String,
}

/// The account owning an [`AppInfo`].
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct AppOwner {
  /// User or organization login.
  pub login: String,
  /// Numeric account id.
  pub id: i64,
}

/// `GET /orgs/{org}` reply, reduced to what `create-app --org` needs.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct OrgInfo {
  /// The organization login, as GitHub spells it.
  pub login: String,
  /// Numeric organization id.
  pub id: i64,
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
