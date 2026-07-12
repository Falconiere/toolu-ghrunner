//! Generic cache HTTP server harness: bind an `axum::Router` and serve it
//! with graceful shutdown. Later steps mount the blob / twirp / v1 / proxy
//! routers onto this; the harness itself knows nothing about handlers.

use std::net::SocketAddr;

use shared::RunnerError;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

/// A running cache HTTP server: owns the serve task and a shutdown signal.
pub struct CacheServer {
  join_handle: JoinHandle<()>,
  shutdown_tx: oneshot::Sender<()>,
  base_url: String,
  addr: SocketAddr,
}

impl CacheServer {
  /// Bind `router` on `bind` and serve it with graceful shutdown.
  ///
  /// `bind` may be `"0.0.0.0:0"` — port `0` picks an ephemeral port, read
  /// back from the listener. `base_url()` always reports a loopback URL,
  /// because that is what gets injected into step env, even when the
  /// listener binds `0.0.0.0`.
  ///
  /// # Errors
  ///
  /// Returns `RunnerError::Cache` if the listener cannot bind `bind` or its
  /// local address cannot be read.
  pub async fn start(router: axum::Router, bind: &str) -> Result<Self, RunnerError> {
    let listener = TcpListener::bind(bind)
      .await
      .map_err(|e| RunnerError::Cache(format!("cache server bind {bind}: {e}")))?;
    let addr = listener
      .local_addr()
      .map_err(|e| RunnerError::Cache(format!("cache server local_addr: {e}")))?;
    let base_url = format!("http://127.0.0.1:{}/", addr.port());

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
      join_handle,
      shutdown_tx,
      base_url,
      addr,
    })
  }

  /// The loopback base URL (`http://127.0.0.1:<port>/`) for step env.
  pub fn base_url(&self) -> &str {
    &self.base_url
  }

  /// The actually-bound socket address (may be `0.0.0.0:<port>`).
  pub fn address(&self) -> SocketAddr {
    self.addr
  }

  /// Signal graceful shutdown and await the serve task.
  pub async fn shutdown(self) {
    let _ = self.shutdown_tx.send(());
    let _ = self.join_handle.await;
  }
}
