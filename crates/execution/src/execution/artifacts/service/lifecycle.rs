//! Artifact service lifecycle (start, base_url, shutdown).

use std::sync::Arc;

use axum::routing::{get, post};
use shared::RunnerError;
use tokio::sync::RwLock;

use super::handlers;
use crate::execution::artifacts::backend::LocalBackend;
use crate::execution::service_lifecycle::ServiceHandle;

pub(super) struct ServiceState {
  pub(super) backend: LocalBackend,
  pub(super) bearer_token: String,
  pub(super) artifact_registry: RwLock<Vec<RegistryEntry>>,
}

pub(super) struct RegistryEntry {
  pub(super) id: u64,
  pub(super) name: String,
}

/// Local HTTP service mimicking GitHub's artifact API at `ACTIONS_RUNTIME_URL`.
pub struct ArtifactService {
  handle: ServiceHandle,
}

impl ArtifactService {
  /// Start the artifact service on a random localhost port.
  ///
  /// # Errors
  ///
  /// Returns `RunnerError::Io` if the TCP listener fails to bind.
  pub async fn start(backend: LocalBackend, bearer_token: String) -> Result<Self, RunnerError> {
    let state = Arc::new(ServiceState {
      backend,
      bearer_token,
      artifact_registry: RwLock::new(Vec::new()),
    });

    let app = axum::Router::new()
      .route(
        "/_apis/pipelines/workflows/:run_id/artifacts",
        post(handlers::handle_create)
          .patch(handlers::handle_upload_or_finalize)
          .get(handlers::handle_list),
      )
      .route(
        "/_apis/pipelines/workflows/:run_id/artifacts/:artifact_id/download",
        get(handlers::handle_download),
      )
      .with_state(state);

    let handle = ServiceHandle::start(app).await?;
    Ok(Self { handle })
  }

  /// The base URL for `ACTIONS_RUNTIME_URL`.
  pub fn base_url(&self) -> String {
    self.handle.base_url()
  }

  /// Gracefully shut down the service.
  pub async fn shutdown(mut self) {
    self.handle.shutdown().await;
  }
}
