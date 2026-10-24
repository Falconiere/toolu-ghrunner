//! Two registrations' local services never clash on `service_bind` (#208).
//!
//! `offline` / `accelerated` jobs start their cache server on
//! `cache::server::ephemeral_bind(&config.service_bind)` (`execution::job_runner`):
//! port 0, so the OS picks a free port for each. Two registrations sharing one
//! `service_bind` — the default included — run jobs concurrently under one
//! `run --all` without EADDRINUSE. Real listeners, real HTTP.

use axum::Router;
use axum::routing::get;
use cache::server::{CacheServer, ephemeral_bind};
use shared::RunnerConfig;

#[tokio::test]
async fn two_servers_on_one_service_bind_get_distinct_ports()
-> Result<(), Box<dyn std::error::Error>> {
  let bind = ephemeral_bind(RunnerConfig::DEFAULT_SERVICE_BIND);
  assert_eq!(bind, "0.0.0.0:0", "the production bind pins no port");
  let first =
    CacheServer::start(Router::new().route("/", get(|| async { "first" })), &bind).await?;
  let second =
    CacheServer::start(Router::new().route("/", get(|| async { "second" })), &bind).await?;
  assert_ne!(first.address().port(), second.address().port());

  let client = reqwest::Client::new();
  for (server, body) in [(&first, "first"), (&second, "second")] {
    let reply = client.get(server.base_url()).send().await?;
    assert!(reply.status().is_success());
    assert_eq!(reply.text().await?, body);
  }
  first.shutdown().await;
  second.shutdown().await;
  Ok(())
}
