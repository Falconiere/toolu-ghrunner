//! Real-data tests for the OIDC token service (AC #7).
//!
//! Spins up `OidcServer` in `Local` mode on `127.0.0.1:0`, mints a
//! token by POSTing to the same endpoint a real GitHub Actions
//! `ACTIONS_ID_TOKEN_REQUEST_TOKEN` step would hit, and verifies:
//! - the response carries a well-formed JWT,
//! - the JWT decodes to claims matching the job context,
//! - the audience can be overridden via the `audience` query param,
//! - bearer-token auth is enforced.
//!
//! Uses the runner's own axum server (no `wiremock` for the main path)
//! so the test exercises the real request handler, not a mock of it.

use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use execution::execution::oidc::{OidcConfig, OidcJobContext, OidcServer};
use serde_json::Value;

fn sample_job_context() -> OidcJobContext {
  OidcJobContext {
    repository: "Falconiere/toolu-ghrunner".to_owned(),
    repository_owner: "Falconiere".to_owned(),
    actor: "octocat".to_owned(),
    event_name: "push".to_owned(),
    git_ref: "refs/heads/main".to_owned(),
    sha: "abc1234567890".to_owned(),
    workflow: "ci.yml".to_owned(),
    run_id: "99999".to_owned(),
    run_number: "42".to_owned(),
    run_attempt: "1".to_owned(),
  }
}

/// Assert that `claims` carries `key` as a string equal to `expected`.
/// Pattern: read the value into `actual`, assert the `Option::is_some`
/// runtime condition (so clippy doesn't see a constant-true assertion),
/// then compare.
fn assert_str(claims: &Value, key: &str, expected: &str) {
  let actual = claims.get(key).and_then(Value::as_str);
  let found = actual.is_some();
  let actual = actual.unwrap_or("");
  assert!(found, "claim {key} missing in {claims}");
  assert_eq!(actual, expected, "claim {key} mismatch");
}

#[tokio::test]
async fn oidc_server_mints_a_jwt_with_default_audience() {
  let server = OidcServer::start(
    OidcConfig::local(
      (0..32u8).collect(),
      "https://toolu-runner.example/oidc".to_owned(),
    ),
    "test-bearer-token".to_owned(),
    sample_job_context(),
  )
  .await
  .expect("start");

  let client = reqwest::Client::new();
  let resp = client
    .post(server.request_url())
    .bearer_auth("test-bearer-token")
    .json(&serde_json::json!({}))
    .send()
    .await
    .expect("POST request_token");

  assert_eq!(resp.status(), 200, "expected 200 OK");
  let body: Value = resp.json().await.expect("parse JSON body");
  let token = body.get("value").and_then(Value::as_str);
  let has_token = token.is_some();
  let token = token.unwrap_or("");
  assert!(has_token, "response missing .value: {body}");

  let claims = decode_jwt_payload(token);
  assert_str(&claims, "iss", "https://toolu-runner.example/oidc");
  assert_str(&claims, "aud", "api://AzureADTokenExchange");
  assert_str(&claims, "repository", "Falconiere/toolu-ghrunner");
  assert_str(&claims, "repository_owner", "Falconiere");
  assert_str(&claims, "actor", "octocat");
  assert_str(&claims, "event_name", "push");
  assert_str(&claims, "ref", "refs/heads/main");
  assert_str(&claims, "sha", "abc1234567890");
  assert_str(&claims, "workflow", "ci.yml");
  assert_str(&claims, "run_id", "99999");
  assert_str(&claims, "run_number", "42");
  assert_str(&claims, "run_attempt", "1");

  let iat = claims.get("iat").and_then(Value::as_u64).unwrap_or(0);
  let exp = claims.get("exp").and_then(Value::as_u64).unwrap_or(0);
  let nbf = claims.get("nbf").and_then(Value::as_u64).unwrap_or(0);
  let has_iat = claims.get("iat").and_then(Value::as_u64).is_some();
  let has_exp = claims.get("exp").and_then(Value::as_u64).is_some();
  let has_nbf = claims.get("nbf").and_then(Value::as_u64).is_some();
  assert!(has_iat, "iat missing in {claims}");
  assert!(has_exp, "exp missing in {claims}");
  assert!(has_nbf, "nbf missing in {claims}");
  assert!(exp > iat, "exp must be after iat");
  assert_eq!(exp - iat, 600, "exp - iat should be 600s (10 minutes)");
  assert_eq!(nbf, iat, "nbf should equal iat");

  let jti = claims.get("jti").and_then(Value::as_str).unwrap_or("");
  let has_jti = !jti.is_empty();
  assert!(has_jti, "jti empty");
  let parsed = uuid::Uuid::parse_str(jti);
  let is_uuid = parsed.is_ok();
  assert!(is_uuid, "jti should parse as UUID, got {jti}");

  server.shutdown().await;
}

#[tokio::test]
async fn oidc_server_respects_audience_query_param() {
  let server = OidcServer::start(
    OidcConfig::local(
      (0..32u8).collect(),
      "https://toolu-runner.example/oidc".to_owned(),
    ),
    "test-bearer-token".to_owned(),
    sample_job_context(),
  )
  .await
  .expect("start");

  let client = reqwest::Client::new();
  let url = format!("{}&audience=https://my-cloud.example", server.request_url());
  let resp = client
    .post(&url)
    .bearer_auth("test-bearer-token")
    .json(&serde_json::json!({}))
    .send()
    .await
    .expect("POST with audience");

  assert_eq!(resp.status(), 200);
  let body: Value = resp.json().await.expect("parse body");
  let token = body.get("value").and_then(Value::as_str).unwrap_or("");
  let has_token = !token.is_empty();
  assert!(has_token, "missing .value: {body}");
  let claims = decode_jwt_payload(token);
  assert_str(&claims, "aud", "https://my-cloud.example");

  server.shutdown().await;
}

#[tokio::test]
async fn oidc_server_rejects_missing_bearer_token() {
  let server = OidcServer::start(
    OidcConfig::local(
      (0..32u8).collect(),
      "https://toolu-runner.example/oidc".to_owned(),
    ),
    "test-bearer-token".to_owned(),
    sample_job_context(),
  )
  .await
  .expect("start");

  let client = reqwest::Client::new();
  let resp = client
    .post(server.request_url())
    .json(&serde_json::json!({}))
    .send()
    .await
    .expect("POST no auth");
  assert_eq!(resp.status(), 401, "missing bearer should be 401");

  server.shutdown().await;
}

#[tokio::test]
async fn oidc_server_rejects_wrong_bearer_token() {
  let server = OidcServer::start(
    OidcConfig::local(
      (0..32u8).collect(),
      "https://toolu-runner.example/oidc".to_owned(),
    ),
    "test-bearer-token".to_owned(),
    sample_job_context(),
  )
  .await
  .expect("start");

  let client = reqwest::Client::new();
  let resp = client
    .post(server.request_url())
    .bearer_auth("not-the-real-token")
    .json(&serde_json::json!({}))
    .send()
    .await
    .expect("POST wrong bearer");
  assert_eq!(resp.status(), 401);

  server.shutdown().await;
}

#[tokio::test]
async fn oidc_server_address_is_localhost_loopback() {
  let server = OidcServer::start(
    OidcConfig::local(vec![0u8; 32], "https://issuer".to_owned()),
    "tok".to_owned(),
    sample_job_context(),
  )
  .await
  .expect("start");
  let addr = server.address();
  assert_eq!(addr.ip().to_string(), "127.0.0.1");
  assert!(addr.port() > 0, "random port assigned");
  let req_url = server.request_url();
  let has_api_version = req_url.contains("api-version=1");
  assert!(has_api_version, "got: {req_url}");
  server.shutdown().await;
}

#[tokio::test]
async fn oidc_server_shuts_down_cleanly() {
  let server = OidcServer::start(
    OidcConfig::local(vec![0u8; 32], "https://issuer".to_owned()),
    "tok".to_owned(),
    sample_job_context(),
  )
  .await
  .expect("start");
  let client = reqwest::Client::new();
  let _ = client
    .post(server.request_url())
    .bearer_auth("tok")
    .json(&serde_json::json!({}))
    .timeout(Duration::from_secs(2))
    .send()
    .await
    .expect("first request");
  server.shutdown().await;
  tokio::time::sleep(Duration::from_millis(50)).await;
}

/// Decode the payload of a (header-payload-signature) JWT into a JSON
/// value. The token is signed with a local HS256 key the test owns, so
/// we don't verify the signature — we just inspect the claims.
fn decode_jwt_payload(token: &str) -> Value {
  let mut parts = token.split('.');
  let header_present = parts.next().is_some();
  let payload_b64 = parts.next().unwrap_or("");
  let sig_present = parts.next().is_some();
  let all_present = header_present && !payload_b64.is_empty() && sig_present;
  assert!(all_present, "JWT missing parts: {token}");
  let bytes = B64.decode(payload_b64.as_bytes()).unwrap_or_default();
  let parsed: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
  parsed
}
