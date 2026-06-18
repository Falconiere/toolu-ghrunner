//! HTTP route handlers for the artifact service.

use std::sync::Arc;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use serde::Deserialize;

use super::lifecycle::ServiceState;
use crate::execution::artifacts::backend::ArtifactBackend;
use crate::execution::service_auth::validate_bearer;
use crate::execution::service_lifecycle::{
  error_response, parse_content_range_start, unauthorized_response,
};

#[derive(Deserialize)]
pub(super) struct ArtifactQuery {
  #[serde(rename = "api-version")]
  _api_version: Option<String>,
  #[serde(rename = "artifactName")]
  artifact_name: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct CreateBody {
  #[serde(rename = "Name")]
  name: String,
}

/// POST -- create artifact container.
pub(super) async fn handle_create(
  State(state): State<Arc<ServiceState>>,
  headers: HeaderMap,
  Path(run_id): Path<String>,
  Json(body): Json<CreateBody>,
) -> impl IntoResponse {
  if validate_bearer(&headers, &state.bearer_token).is_err() {
    return unauthorized_response();
  }

  match state.backend.create_container(&run_id, &body.name).await {
    Ok(container_id) => {
      let mut registry = state.artifact_registry.write().await;
      let id = registry.len() as u64 + 1;
      registry.push(super::lifecycle::RegistryEntry {
        id,
        name: body.name.clone(),
      });
      (
        StatusCode::OK,
        Json(serde_json::json!({"containerId": container_id, "name": body.name})),
      )
        .into_response()
    },
    Err(e) => (
      StatusCode::INTERNAL_SERVER_ERROR,
      Json(serde_json::json!({"error": e.to_string()})),
    )
      .into_response(),
  }
}

/// PATCH -- upload chunk or finalize.
pub(super) async fn handle_upload_or_finalize(
  State(state): State<Arc<ServiceState>>,
  headers: HeaderMap,
  Path(run_id): Path<String>,
  Query(params): Query<ArtifactQuery>,
  body: Bytes,
) -> impl IntoResponse {
  if validate_bearer(&headers, &state.bearer_token).is_err() {
    return unauthorized_response();
  }

  let Some(artifact_name) = params.artifact_name else {
    return (
      StatusCode::BAD_REQUEST,
      Json(serde_json::json!({"error": "artifactName required"})),
    )
      .into_response();
  };

  let is_finalize = headers
    .get("x-actions-results-cfs-finalize")
    .and_then(|v| v.to_str().ok())
    .is_some_and(|v| v == "true");

  if is_finalize {
    match state.backend.finalize(&run_id, &artifact_name).await {
      Ok(()) => (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response(),
      Err(e) => error_response(&e),
    }
  } else {
    let chunk_index = u32::try_from(parse_content_range_start(&headers)).unwrap_or(u32::MAX);
    let result = state
      .backend
      .upload_chunk(&run_id, &artifact_name, chunk_index, body.to_vec())
      .await;
    match result {
      Ok(()) => (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response(),
      Err(e) => error_response(&e),
    }
  }
}

/// GET -- list artifacts.
pub(super) async fn handle_list(
  State(state): State<Arc<ServiceState>>,
  headers: HeaderMap,
  Path(run_id): Path<String>,
) -> impl IntoResponse {
  if validate_bearer(&headers, &state.bearer_token).is_err() {
    return unauthorized_response();
  }

  match state.backend.list(&run_id).await {
    Ok(entries) => {
      let value: Vec<serde_json::Value> = entries
        .iter()
        .map(|e| serde_json::json!({"id": e.id, "name": e.name, "size": e.size}))
        .collect();
      (
        StatusCode::OK,
        Json(serde_json::json!({"count": value.len(), "value": value})),
      )
        .into_response()
    },
    Err(e) => (
      StatusCode::INTERNAL_SERVER_ERROR,
      Json(serde_json::json!({"error": e.to_string()})),
    )
      .into_response(),
  }
}

/// GET -- download artifact by ID.
pub(super) async fn handle_download(
  State(state): State<Arc<ServiceState>>,
  headers: HeaderMap,
  Path((run_id, artifact_id)): Path<(String, u64)>,
) -> impl IntoResponse {
  if validate_bearer(&headers, &state.bearer_token).is_err() {
    return (StatusCode::UNAUTHORIZED, axum::body::Body::empty()).into_response();
  }

  let artifact_name = find_artifact_name(&state, &run_id, artifact_id).await;
  let Some(name) = artifact_name else {
    return (StatusCode::NOT_FOUND, axum::body::Body::empty()).into_response();
  };

  match state.backend.download(&run_id, &name).await {
    Ok(data) => (StatusCode::OK, axum::body::Body::from(data)).into_response(),
    Err(_) => (StatusCode::NOT_FOUND, axum::body::Body::empty()).into_response(),
  }
}

async fn find_artifact_name(
  state: &ServiceState,
  run_id: &str,
  artifact_id: u64,
) -> Option<String> {
  let registry = state.artifact_registry.read().await;
  if let Some(entry) = registry.iter().find(|e| e.id == artifact_id) {
    return Some(entry.name.clone());
  }
  drop(registry);

  let entries = state.backend.list(run_id).await.ok()?;
  entries
    .iter()
    .find(|e| e.id == artifact_id)
    .map(|e| e.name.clone())
}
