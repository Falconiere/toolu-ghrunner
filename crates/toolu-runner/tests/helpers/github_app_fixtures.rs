//! Shared #201 fixtures: the protocol crate's real test RSA key, the GitHub
//! App reply captures in `fixtures/github_app/` (live 401/404 captures and
//! GitHub's documented success examples, see each file's `provenance`), JWT
//! verification, a real `github-app.json` writer, a peer that echoes the
//! bearer it was sent, and (#202) a peer that answers only a valid App JWT.
//!
//! Declared once in `tests/main.rs`; test files use `crate::github_app_fixtures`.

use std::error::Error;
use std::path::{Path, PathBuf};

use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde_json::Value;
use wiremock::{Request, Respond, ResponseTemplate};

/// The 2048-bit test key (generated for these tests, registered to no App).
pub const PRIVATE_PEM: &str = include_str!("../../../protocol/tests/fixtures/app_jwt_test_key.pem");
/// Its public half, which verifies every App JWT the runner sends.
pub const PUBLIC_PEM: &str =
  include_str!("../../../protocol/tests/fixtures/app_jwt_test_key.pub.pem");
/// The App id written to the test `github-app.json`.
pub const APP_ID: i64 = 424_242;
/// The App slug written to the test `github-app.json`.
pub const APP_SLUG: &str = "toolu-runner-test-app";
/// The sanitized token in `access_tokens_201.json`.
pub const INSTALLATION_TOKEN: &str = "ghs_EXAMPLE-installation-token-201";

/// A capture fixture's JSON document.
fn fixture(name: &str) -> Result<Value, Box<dyn Error>> {
  let path = Path::new(env!("CARGO_MANIFEST_DIR"))
    .join("tests/fixtures/github_app")
    .join(name);
  Ok(serde_json::from_str(&std::fs::read_to_string(path)?)?)
}

/// A capture's `response.body`.
pub fn capture_body(name: &str) -> Result<String, Box<dyn Error>> {
  Ok(
    fixture(name)?
      .pointer("/response/body")
      .and_then(Value::as_str)
      .ok_or("capture has no response.body")?
      .to_owned(),
  )
}

/// Replay a capture: its status, content type and body.
pub fn capture(name: &str) -> Result<ResponseTemplate, Box<dyn Error>> {
  let status = fixture(name)?
    .pointer("/response/status")
    .and_then(Value::as_u64)
    .ok_or("capture has no response.status")?;
  Ok(
    ResponseTemplate::new(u16::try_from(status)?)
      .insert_header("content-type", "application/json; charset=utf-8")
      .set_body_string(capture_body(name)?),
  )
}

/// The documented `access_tokens` 201 with `expires_at` rewritten (the
/// documented 2016 instant would make every token single-use).
pub fn token_reply(expires_at: &str) -> Result<ResponseTemplate, Box<dyn Error>> {
  let mut body: Value = serde_json::from_str(&capture_body("access_tokens_201.json")?)?;
  let object = body.as_object_mut().ok_or("token reply is not an object")?;
  object.insert("expires_at".to_owned(), Value::from(expires_at));
  Ok(ResponseTemplate::new(201).set_body_json(body))
}

/// An RFC 3339 instant `secs` from now.
pub fn rfc3339_in(secs: i64) -> String {
  (chrono::Utc::now() + chrono::Duration::seconds(secs)).to_rfc3339()
}

/// Verify an `Authorization: Bearer <jwt>` value as an App JWT for
/// [`APP_ID`] signed by the test key, unexpired; returns its claims.
pub fn verify_app_jwt(authorization: &str) -> Result<Value, Box<dyn Error>> {
  let jwt = authorization
    .strip_prefix("Bearer ")
    .ok_or("not a bearer authorization")?;
  let key = DecodingKey::from_rsa_pem(PUBLIC_PEM.as_bytes())?;
  let mut validation = Validation::new(Algorithm::RS256);
  validation.set_issuer(&[APP_ID.to_string()]);
  Ok(jsonwebtoken::decode::<Value>(jwt, &key, &validation)?.claims)
}

/// The `Authorization` header of every request `server` saw at `path`.
pub async fn authorizations(
  server: &wiremock::MockServer,
  path: &str,
) -> Result<Vec<String>, Box<dyn Error>> {
  let requests = server
    .received_requests()
    .await
    .ok_or("request recording disabled")?;
  requests
    .iter()
    .filter(|request| request.url.path() == path)
    .map(|request| {
      request
        .headers
        .get("authorization")
        .ok_or_else(|| format!("request to {path} had no authorization").into())
        .and_then(|value| Ok(value.to_str()?.to_owned()))
    })
    .collect()
}

/// Write a real `github-app.json` (0600) for `host` under `home`, as
/// `create-app` does, and return its path.
pub fn write_app(home: &Path, host: &str) -> Result<PathBuf, Box<dyn Error>> {
  let app = config::app_store::StoredApp {
    host: host.to_owned(),
    app_id: APP_ID,
    slug: APP_SLUG.to_owned(),
    owner: "octo".to_owned(),
    client_id: "Iv1.toolutest0000000".to_owned(),
    client_secret: "toolu-test-client-secret-201".to_owned(),
    webhook_secret: None,
    pem: PRIVATE_PEM.to_owned(),
    html_url: format!("https://{host}/apps/{APP_SLUG}"),
    created_at: "2026-10-23T00:00:00Z".to_owned(),
  };
  config::app_store::save_app(home, &app)?;
  Ok(config::app_store::app_path(home))
}

/// A reply with this status whose body echoes the request's `Authorization`
/// header, as a misbehaving proxy might: whatever quotes the body must mask
/// the bearer.
pub struct EchoAuthorization(pub u16);

impl Respond for EchoAuthorization {
  fn respond(&self, request: &Request) -> ResponseTemplate {
    let authorization = request
      .headers
      .get("authorization")
      .map_or("<no authorization header>", |value| {
        value.to_str().unwrap_or("<non-UTF-8 authorization header>")
      });
    ResponseTemplate::new(self.0).set_body_string(format!("upstream echoed: {authorization}"))
  }
}

/// A reply GitHub gives only to a valid App JWT: when the request's bearer
/// verifies as an App JWT for [`APP_ID`] signed by the test key, `ok`;
/// otherwise the live `app_401_bad_jwt.json` capture (#202) — what GitHub
/// answers a key the App does not hold.
pub struct AppJwtGate(pub ResponseTemplate);

impl Respond for AppJwtGate {
  fn respond(&self, request: &Request) -> ResponseTemplate {
    let verified = request
      .headers
      .get("authorization")
      .and_then(|value| value.to_str().ok())
      .is_some_and(|authorization| verify_app_jwt(authorization).is_ok());
    if verified {
      return self.0.clone();
    }
    capture("app_401_bad_jwt.json").unwrap_or_else(|e| {
      ResponseTemplate::new(500).set_body_string(format!("fixture unreadable: {e}"))
    })
  }
}

/// A documented reply's JSON body with `edit` applied, as a 200 (#202: the
/// documented examples name `octocat`; the tests rewrite the account or App
/// id they key on).
pub fn edited_reply(
  name: &str,
  edit: impl FnOnce(&mut Value),
) -> Result<ResponseTemplate, Box<dyn Error>> {
  let mut body: Value = serde_json::from_str(&capture_body(name)?)?;
  edit(&mut body);
  Ok(ResponseTemplate::new(200).set_body_json(body))
}
