//! GitHub App REST calls (#201): find the App's installation for a
//! registration and mint an installation token for it, authenticated with
//! the App JWT (`protocol::app_jwt`). #202 adds what `create-app` needs:
//! `GET /app` (proves a key), the unauthenticated org check, and the
//! selected-repository count.
//!
//! [`api_failure`] is `register::mint_failure`'s rule: transport, 429 and
//! 5xx are `RunnerError::Network` (the loop backs off); every other non-2xx
//! is `RunnerError::Auth` (fatal) — including 404, which is how GitHub
//! answers a deleted App (`Integration not found`, live capture) or a
//! removed installation.

use std::time::Duration;

use protocol::app_jwt::{AppInfo, Installation, InstallationToken, OrgInfo};
use reqwest::StatusCode;
use serde::de::DeserializeOwned;
use shared::RunnerError;

use super::register::RunnerScope;

/// Longest a cached installation token is reused after minting (GitHub's
/// tokens live one hour).
pub const REUSE_MAX_SECS: i64 = 3000;

/// Margin kept before a cached token's `expires_at`.
pub const REUSE_MARGIN_SECS: i64 = 300;

/// The body GitHub sends when the JWT's `iss` names no App.
const UNKNOWN_APP: &str = "Integration not found";

/// Unix second until which a token minted at `minted_unix` with `expires_at`
/// (RFC 3339) may be reused: the earlier of `minted + 50 min` and
/// `expires_at − 5 min`. `None` when `expires_at` does not parse, so the
/// token is used once.
pub fn reuse_deadline(expires_at: &str, minted_unix: i64) -> Option<i64> {
  let expires = chrono::DateTime::parse_from_rfc3339(expires_at)
    .ok()?
    .timestamp();
  Some(
    minted_unix
      .saturating_add(REUSE_MAX_SECS)
      .min(expires.saturating_sub(REUSE_MARGIN_SECS)),
  )
}

/// Add the headers every GitHub REST call carries plus the bearer and a
/// per-request timeout.
pub(crate) fn github_request(
  request: reqwest::RequestBuilder,
  bearer: &str,
  timeout: Duration,
) -> reqwest::RequestBuilder {
  github_headers(request.bearer_auth(bearer), timeout)
}

/// Add the headers every GitHub REST call carries and a per-request timeout,
/// with no credential.
fn github_headers(request: reqwest::RequestBuilder, timeout: Duration) -> reqwest::RequestBuilder {
  request
    .timeout(timeout)
    .header("Accept", "application/vnd.github+json")
    .header("X-GitHub-Api-Version", "2022-11-28")
    // GitHub's REST API rejects requests without a User-Agent (403).
    .header(
      "User-Agent",
      concat!("toolu-runner/", env!("CARGO_PKG_VERSION")),
    )
}

/// Send `request` and read the status and body; a transport failure is
/// `RunnerError::Network`.
pub(crate) async fn send(
  request: reqwest::RequestBuilder,
  what: &str,
) -> Result<(StatusCode, String), RunnerError> {
  let response = request
    .send()
    .await
    .map_err(|e| RunnerError::Network(format!("{what} request failed: {e}")))?;
  let status = response.status();
  let body = response
    .text()
    .await
    .map_err(|e| RunnerError::Network(format!("reading the {what} reply failed: {e}")))?;
  Ok((status, body))
}

/// Map a non-2xx reply of `what`: 429 and 5xx are transient
/// (`RunnerError::Network`), every other status is permanent
/// (`RunnerError::Auth`). GitHub's body is quoted in both.
pub(crate) fn api_failure(what: &str, status: StatusCode, body: &str) -> RunnerError {
  if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
    return RunnerError::Network(format!(
      "{what} failed with status {status} (transient — retried after a pause): {body}"
    ));
  }
  RunnerError::Auth(format!("{what} failed with status {status}: {body}"))
}

/// Decode a 2xx reply of `what`, or map the failure with [`api_failure`].
pub(crate) fn decode<T: DeserializeOwned>(
  what: &str,
  status: StatusCode,
  body: &str,
) -> Result<T, RunnerError> {
  if !status.is_success() {
    return Err(api_failure(what, status, body));
  }
  serde_json::from_str(body)
    .map_err(|e| RunnerError::Protocol(format!("{what} reply did not parse: {e}")))
}

/// The App's installation that covers `scope`: `GET <api>/repos/{o}/{r}/installation`
/// or `GET <api>/orgs/{org}/installation`, authenticated with the App `jwt`.
///
/// `Ok(None)` when GitHub answers 404 for an App it knows: the App is not
/// installed on that repository or organization.
///
/// # Errors
///
/// `RunnerError::Auth` for a 404 `Integration not found` (the App itself is
/// unknown: deleted, or a wrong `app_id` / key) and every other permanent
/// status; `RunnerError::Network` for transport / 429 / 5xx;
/// `RunnerError::Protocol` for an unparseable 200.
pub async fn find_installation(
  client: &reqwest::Client,
  api_base: &str,
  jwt: &str,
  scope: &RunnerScope,
  timeout: Duration,
) -> Result<Option<Installation>, RunnerError> {
  let url = format!("{api_base}/{}/installation", scope.api_path());
  let what = "GitHub App installation lookup";
  let (status, body) = send(github_request(client.get(&url), jwt, timeout), what).await?;
  if status == StatusCode::NOT_FOUND {
    if body.contains(UNKNOWN_APP) {
      return Err(RunnerError::Auth(format!(
        "GitHub does not know this App ({what}: 404 {UNKNOWN_APP}) — it was deleted, or the \
         app_id / private key in github-app.json is wrong"
      )));
    }
    return Ok(None);
  }
  decode(what, status, &body).map(Some)
}

/// Every installation of the App: `GET <api>/app/installations?per_page=100`
/// (the first 100: enough to name where the App IS installed, and a private
/// App — what `create-app` makes — installs only on its owner).
///
/// # Errors
///
/// As [`find_installation`], with a 404 always `RunnerError::Auth`.
pub async fn list_installations(
  client: &reqwest::Client,
  api_base: &str,
  jwt: &str,
  timeout: Duration,
) -> Result<Vec<Installation>, RunnerError> {
  let url = format!("{api_base}/app/installations");
  let request = client.get(&url).query(&[("per_page", "100")]);
  let what = "GitHub App installation list";
  let (status, body) = send(github_request(request, jwt, timeout), what).await?;
  decode(what, status, &body)
}

/// Mint an installation token: `POST <api>/app/installations/{id}/access_tokens`.
///
/// # Errors
///
/// `RunnerError::Auth` for 401 (rejected JWT: revoked key, clock skew), 403,
/// 404 (App deleted or installation removed), 422; `RunnerError::Network`
/// for transport / 429 / 5xx; `RunnerError::Protocol` for an unparseable
/// 201 or an empty token.
pub async fn create_installation_token(
  client: &reqwest::Client,
  api_base: &str,
  jwt: &str,
  installation_id: i64,
  timeout: Duration,
) -> Result<InstallationToken, RunnerError> {
  let url = format!("{api_base}/app/installations/{installation_id}/access_tokens");
  let what = "GitHub App installation token";
  let (status, body) = send(github_request(client.post(&url), jwt, timeout), what).await?;
  let token: InstallationToken = decode(what, status, &body)?;
  if token.token.is_empty() {
    return Err(RunnerError::Protocol(format!(
      "{what} reply carried an empty token"
    )));
  }
  Ok(token)
}

/// The App the `jwt` authenticates as: `GET <api>/app`. A 2xx proves the key
/// belongs to the App the JWT's `iss` names.
///
/// # Errors
///
/// `RunnerError::Auth` for 401 (the key does not match: GitHub cannot verify
/// the JWT) or 404 (no such App), quoting GitHub; `RunnerError::Network` for
/// transport / 429 / 5xx; `RunnerError::Protocol` for an unparseable 200.
pub async fn get_app(
  client: &reqwest::Client,
  api_base: &str,
  jwt: &str,
  timeout: Duration,
) -> Result<AppInfo, RunnerError> {
  let url = format!("{api_base}/app");
  let what = "GitHub App lookup";
  let (status, body) = send(github_request(client.get(&url), jwt, timeout), what).await?;
  decode(what, status, &body)
}

/// The organization `org`: `GET <api>/orgs/{org}`, sent with no credential
/// (it runs before any exists). `Ok(None)` on 404: no such organization
/// (a typo, or a user login).
///
/// # Errors
///
/// `RunnerError::Network` for transport / 429 / 5xx; `RunnerError::Auth`
/// for any other non-2xx (e.g. a 403 unauthenticated rate limit);
/// `RunnerError::Protocol` for an unparseable 200.
pub async fn find_org(
  client: &reqwest::Client,
  api_base: &str,
  org: &str,
  timeout: Duration,
) -> Result<Option<OrgInfo>, RunnerError> {
  let url = format!("{api_base}/orgs/{org}");
  let what = "GitHub organization lookup";
  let (status, body) = send(github_headers(client.get(&url), timeout), what).await?;
  if status == StatusCode::NOT_FOUND {
    return Ok(None);
  }
  decode(what, status, &body).map(Some)
}

/// `GET /installation/repositories` reply, reduced to its count.
#[derive(serde::Deserialize)]
struct RepositoryPage {
  total_count: u64,
}

/// How many repositories an installation selected:
/// `GET <api>/installation/repositories?per_page=1` with an installation
/// `token`, read from `total_count`.
///
/// # Errors
///
/// As [`get_app`].
pub async fn installation_repo_count(
  client: &reqwest::Client,
  api_base: &str,
  token: &str,
  timeout: Duration,
) -> Result<u64, RunnerError> {
  let url = format!("{api_base}/installation/repositories");
  let request = client.get(&url).query(&[("per_page", "1")]);
  let what = "GitHub App installation repository count";
  let (status, body) = send(github_request(request, token, timeout), what).await?;
  decode::<RepositoryPage>(what, status, &body).map(|page| page.total_count)
}
