//! Real-data tests for the generic cache HTTP server harness: a real
//! `axum::Router` served over a real TCP socket, driven with `reqwest`.

use axum::routing::get;
use cache::server::CacheServer;

/// Boxed error alias for test helpers that use `?`.
type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

/// A router with a single `GET /_health` returning `200 "ok"`.
fn health_router() -> axum::Router {
  axum::Router::new().route("/_health", get(|| async { "ok" }))
}

/// GET `url` and return its status code and body text.
async fn get_text(url: &str) -> TestResult<(reqwest::StatusCode, String)> {
  let resp = reqwest::get(url).await?;
  let status = resp.status();
  let body = resp.text().await?;
  Ok((status, body))
}

#[tokio::test]
async fn health_serves_then_shutdown_stops() -> TestResult<()> {
  let server = CacheServer::start(health_router(), "127.0.0.1:0").await?;

  let port = server.address().port();
  assert_ne!(port, 0, "bound port should be nonzero");
  let base = server.base_url().to_owned();
  assert!(
    base.starts_with("http://127.0.0.1:"),
    "base_url {base} is not a loopback url"
  );
  assert!(
    base.contains(&port.to_string()),
    "base_url {base} does not contain bound port {port}"
  );

  let url = format!("{base}_health");
  let (status, body) = get_text(&url).await?;
  assert_eq!(status.as_u16(), 200, "health status");
  assert_eq!(body, "ok", "health body");

  server.shutdown().await;

  let after = reqwest::get(&url).await;
  assert!(
    after.is_err(),
    "GET should fail once the server has shut down"
  );
  Ok(())
}

#[tokio::test]
async fn two_servers_get_distinct_ports_and_serve_independently() -> TestResult<()> {
  let one = CacheServer::start(health_router(), "127.0.0.1:0").await?;
  let two = CacheServer::start(health_router(), "127.0.0.1:0").await?;

  assert_ne!(
    one.address().port(),
    two.address().port(),
    "two ephemeral binds should get distinct ports"
  );

  let first = get_text(&format!("{}_health", one.base_url())).await?;
  assert_eq!(first.0.as_u16(), 200, "first server status");
  assert_eq!(first.1, "ok", "first server body");

  let second = get_text(&format!("{}_health", two.base_url())).await?;
  assert_eq!(second.0.as_u16(), 200, "second server status");
  assert_eq!(second.1, "ok", "second server body");

  one.shutdown().await;
  two.shutdown().await;
  Ok(())
}
