//! Cache service lifecycle (start, base_url, shutdown).

use std::sync::Arc;

use axum::routing::{get, post};
use shared::RunnerError;
use tokio::net::TcpListener;

use super::handlers;
use crate::execution::cache::backend::LocalDiskBackend;
use crate::execution::service_lifecycle::ServiceHandle;

pub(super) struct ServiceState {
  pub(super) backend: LocalDiskBackend,
  pub(super) bearer_token: String,
  pub(super) base_url: String,
}

/// Local HTTP service mimicking GitHub's cache API at `ACTIONS_CACHE_URL`.
pub struct CacheService {
  handle: ServiceHandle,
  base_url: String,
}

impl CacheService {
  /// Start the cache service on a random localhost port.
  ///
  /// # Errors
  ///
  /// Returns `RunnerError::Io` if the TCP listener fails to bind.
  pub async fn start(backend: LocalDiskBackend, bearer_token: String) -> Result<Self, RunnerError> {
    let listener = TcpListener::bind("127.0.0.1:0")
      .await
      .map_err(RunnerError::Io)?;
    let address = listener.local_addr().map_err(RunnerError::Io)?;
    let base_url = format!("http://{address}");

    let state = Arc::new(ServiceState {
      backend,
      bearer_token,
      base_url: base_url.clone(),
    });

    let app = axum::Router::new()
      .route("/_apis/artifactcache/cache", get(handlers::handle_lookup))
      .route(
        "/_apis/artifactcache/caches",
        post(handlers::handle_reserve),
      )
      .route(
        "/_apis/artifactcache/caches/:cache_id",
        post(handlers::handle_finalize).patch(handlers::handle_upload_chunk),
      )
      .route(
        "/_apis/artifactcache/download/:cache_id",
        get(handlers::handle_download),
      )
      .with_state(state);

    let handle = ServiceHandle::start_with_listener(listener, app).await?;
    Ok(Self { handle, base_url })
  }

  /// The base URL for `ACTIONS_CACHE_URL`.
  pub fn base_url(&self) -> &str {
    &self.base_url
  }

  /// The socket address.
  pub fn address(&self) -> std::net::SocketAddr {
    self.handle.address()
  }

  /// Gracefully shut down the service.
  pub async fn shutdown(mut self) {
    self.handle.shutdown().await;
  }
}
