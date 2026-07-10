//! Real-data tests for the selective reverse proxy ([`proxied_app`]).
//!
//! No mocks: the "upstream" is a genuine second [`CacheServer`] we control, and
//! every request is driven with a real `reqwest` client over real TCP sockets.
//! The three tests pin the load-bearing contract — unmatched paths (and their
//! `Authorization` header) forward upstream, local routes stay local, and an
//! unreachable upstream `502`s the proxied call while the local cache keeps
//! serving.

use axum::body::to_bytes;
use axum::extract::Request;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use toolu_runner::execution::cache::proxied_app;
use toolu_runner::execution::cache::server::CacheServer;

/// Boxed error alias for test helpers that use `?`.
type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

/// A path served by the OTHER Twirp service at this origin — must forward.
const ARTIFACT_PATH: &str = "/twirp/github.actions.results.api.v1.ArtifactService/CreateArtifact";
/// A cache path the local app owns — must stay local.
const PING_PATH: &str = "/twirp/github.actions.results.api.v1.CacheService/ping";
/// Upper bound on the echoed upstream request body.
const MAX_BODY: usize = 1 << 20;

/// Absolute URL for `path` (which must start with `/`) under a server `base`.
///
/// Not a general URL join: it trims a trailing `/` off `base` so the result
/// holds exactly one separator regardless of `base_url`'s convention.
fn abs_url(base: &str, path: &str) -> String {
  debug_assert!(path.starts_with('/'), "path must be absolute: {path}");
  format!("{}{path}", base.trim_end_matches('/'))
}

/// Upstream `ArtifactService` stand-in: echoes a JSON body carrying the received
/// `Authorization` header and the forwarded body length, proving verbatim reach.
async fn echo_artifact(req: Request) -> Response {
  let (parts, body) = req.into_parts();
  let auth = parts
    .headers
    .get(header::AUTHORIZATION)
    .and_then(|v| v.to_str().ok())
    .unwrap_or("")
    .to_owned();
  let received = to_bytes(body, MAX_BODY).await.map(|b| b.len()).unwrap_or(0);
  let payload = format!(
    "{{\"source\":\"upstream-artifact\",\"authorization\":\"{auth}\",\"received_bytes\":{received}}}"
  );
  ([(header::CONTENT_TYPE, "application/json")], payload).into_response()
}

/// Upstream copy of the cache ping path, returning `500` — so if the proxy ever
/// forwarded a local path, the test would observe this instead of `200 local`.
async fn ping_upstream_500() -> Response {
  (
    StatusCode::INTERNAL_SERVER_ERROR,
    "upstream-ping-should-not-be-served",
  )
    .into_response()
}

/// The upstream stand-in: an `ArtifactService` echo plus a poisoned ping.
fn upstream_router() -> axum::Router {
  axum::Router::new()
    .route(ARTIFACT_PATH, post(echo_artifact))
    .route(PING_PATH, get(ping_upstream_500))
}

/// A minimal local cache app: one route, `GET …/CacheService/ping` → `200`.
fn local_router() -> axum::Router {
  axum::Router::new().route(PING_PATH, get(|| async { "local" }))
}

/// Bind an ephemeral port, read its address, and drop the listener so the
/// address now refuses connections — a genuine dead upstream, no mock.
fn dead_upstream_base() -> TestResult<String> {
  let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
  let addr = listener.local_addr()?;
  drop(listener);
  Ok(format!("http://{addr}"))
}

#[tokio::test]
async fn forwards_unmatched_path_and_authorization() -> TestResult<()> {
  let upstream = CacheServer::start(upstream_router(), "127.0.0.1:0").await?;
  let client = reqwest::Client::new();
  let app = proxied_app(
    local_router(),
    upstream.base_url().to_owned(),
    client.clone(),
  );
  let proxied = CacheServer::start(app, "127.0.0.1:0").await?;

  let resp = client
    .post(abs_url(proxied.base_url(), ARTIFACT_PATH))
    .header("Authorization", "Bearer xyz")
    .header("Content-Type", "application/json")
    .body(r#"{"name":"art"}"#)
    .send()
    .await?;
  let status = resp.status();
  let body = resp.text().await?;

  assert_eq!(
    status.as_u16(),
    200,
    "artifact call should be proxied to upstream, got body: {body}"
  );
  assert!(
    body.contains("upstream-artifact"),
    "response should come from the upstream server: {body}"
  );
  assert!(
    body.contains("Bearer xyz"),
    "Authorization header must pass through untouched: {body}"
  );

  proxied.shutdown().await;
  upstream.shutdown().await;
  Ok(())
}

#[tokio::test]
async fn local_routes_stay_local() -> TestResult<()> {
  let upstream = CacheServer::start(upstream_router(), "127.0.0.1:0").await?;
  let client = reqwest::Client::new();
  let app = proxied_app(
    local_router(),
    upstream.base_url().to_owned(),
    client.clone(),
  );
  let proxied = CacheServer::start(app, "127.0.0.1:0").await?;

  let resp = client
    .get(abs_url(proxied.base_url(), PING_PATH))
    .send()
    .await?;
  let status = resp.status();
  let body = resp.text().await?;

  assert_eq!(
    status.as_u16(),
    200,
    "local ping must be served locally, not forwarded (upstream returns 500)"
  );
  assert_eq!(body, "local", "local route body, not the upstream 500 body");

  proxied.shutdown().await;
  upstream.shutdown().await;
  Ok(())
}

#[tokio::test]
async fn upstream_down_isolates_cache_from_artifacts() -> TestResult<()> {
  let dead = dead_upstream_base()?;
  let client = reqwest::Client::new();
  let app = proxied_app(local_router(), dead, client.clone());
  let proxied = CacheServer::start(app, "127.0.0.1:0").await?;

  let artifact = client
    .post(abs_url(proxied.base_url(), ARTIFACT_PATH))
    .header("Authorization", "Bearer xyz")
    .body("{}")
    .send()
    .await?;
  assert_eq!(
    artifact.status().as_u16(),
    502,
    "an unreachable upstream must 502 the proxied artifact call"
  );

  let ping = client
    .get(abs_url(proxied.base_url(), PING_PATH))
    .send()
    .await?;
  let ping_status = ping.status();
  let ping_body = ping.text().await?;
  assert_eq!(
    ping_status.as_u16(),
    200,
    "local cache must keep serving even with upstream down"
  );
  assert_eq!(ping_body, "local", "local cache body with upstream down");

  proxied.shutdown().await;
  Ok(())
}
