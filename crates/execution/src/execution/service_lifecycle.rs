//! Shared lifecycle management for local HTTP micro-services (artifact, cache, OIDC).

use std::net::SocketAddr;

use axum::Json;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use shared::RunnerError;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

/// Manages the lifecycle of a local axum HTTP service.
pub struct ServiceHandle {
  address: SocketAddr,
  shutdown_tx: Option<oneshot::Sender<()>>,
  join_handle: Option<tokio::task::JoinHandle<()>>,
}

impl ServiceHandle {
  /// Spawn a router on an already-bound listener.
  ///
  /// # Errors
  ///
  /// Returns `RunnerError::Io` if reading the listener's local address fails.
  pub async fn start_with_listener(
    listener: TcpListener,
    router: axum::Router,
  ) -> Result<Self, RunnerError> {
    let address = listener.local_addr().map_err(RunnerError::Io)?;
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let join_handle = tokio::spawn(async move {
      axum::serve(listener, router)
        .with_graceful_shutdown(async {
          let _ = shutdown_rx.await;
        })
        .await
        .ok();
    });

    Ok(Self {
      address,
      shutdown_tx: Some(shutdown_tx),
      join_handle: Some(join_handle),
    })
  }

  /// Bind on a random localhost port and spawn.
  ///
  /// # Errors
  ///
  /// Returns `RunnerError::Io` if binding the listener or reading its
  /// address fails.
  pub async fn start(router: axum::Router) -> Result<Self, RunnerError> {
    let listener = TcpListener::bind("127.0.0.1:0")
      .await
      .map_err(RunnerError::Io)?;
    Self::start_with_listener(listener, router).await
  }

  pub fn address(&self) -> SocketAddr {
    self.address
  }

  pub fn base_url(&self) -> String {
    format!("http://{}", self.address)
  }

  /// Gracefully shut down the service.
  pub async fn shutdown(&mut self) {
    if let Some(tx) = self.shutdown_tx.take() {
      let _ = tx.send(());
    }
    if let Some(handle) = self.join_handle.take() {
      let _ = handle.await;
    }
  }
}

/// Standard 401 JSON response for failed bearer auth.
pub fn unauthorized_response() -> axum::response::Response {
  (
    StatusCode::UNAUTHORIZED,
    Json(serde_json::json!({"error": "unauthorized"})),
  )
    .into_response()
}

/// Standard 500 JSON response wrapping a `RunnerError`.
pub fn error_response(e: &RunnerError) -> axum::response::Response {
  (
    StatusCode::INTERNAL_SERVER_ERROR,
    Json(serde_json::json!({"error": e.to_string()})),
  )
    .into_response()
}

/// Parse the byte offset from a `Content-Range: bytes START-END/TOTAL` header.
/// Returns `u64` — callers that need `u32` can truncate with `as u32`.
pub fn parse_content_range_start(headers: &axum::http::HeaderMap) -> u64 {
  headers
    .get("Content-Range")
    .and_then(|v| v.to_str().ok())
    .and_then(|range| {
      let after_bytes = range.strip_prefix("bytes ")?;
      let dash = after_bytes.find('-')?;
      after_bytes.get(..dash)?.parse::<u64>().ok()
    })
    .unwrap_or(0)
}
