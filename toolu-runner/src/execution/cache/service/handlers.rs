//! HTTP route handlers for the cache service.

use std::sync::Arc;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use serde::Deserialize;

use super::lifecycle::ServiceState;
use crate::execution::cache::backend::CacheBackend;
use crate::execution::service_auth::validate_bearer;
use crate::execution::service_lifecycle::{
  error_response, parse_content_range_start, unauthorized_response,
};

#[derive(Deserialize)]
pub(super) struct LookupParams {
  keys: String,
  version: String,
}

/// GET /_apis/artifactcache/cache -- lookup by key(s) and version.
pub(super) async fn handle_lookup(
  State(state): State<Arc<ServiceState>>,
  headers: HeaderMap,
  Query(params): Query<LookupParams>,
) -> impl IntoResponse {
  if validate_bearer(&headers, &state.bearer_token).is_err() {
    return unauthorized_response();
  }

  let keys: Vec<&str> = params.keys.split(',').map(str::trim).collect();

  for key in &keys {
    match state.backend.lookup(key, &params.version).await {
      Ok(Some(entry)) => {
        let archive_url = format!(
          "{}/_apis/artifactcache/download/{}",
          state.base_url, entry.id
        );
        return (
          StatusCode::OK,
          Json(serde_json::json!({
            "cacheKey": entry.key,
            "scope": "refs/heads/main",
            "archiveLocation": archive_url,
          })),
        )
          .into_response();
      },
      Ok(None) => continue,
      Err(e) => return error_response(&e),
    }
  }

  StatusCode::NO_CONTENT.into_response()
}

#[derive(Deserialize)]
pub(super) struct ReserveBody {
  key: String,
  version: String,
}

/// POST /_apis/artifactcache/caches -- reserve cache entry.
pub(super) async fn handle_reserve(
  State(state): State<Arc<ServiceState>>,
  headers: HeaderMap,
  Json(body): Json<ReserveBody>,
) -> impl IntoResponse {
  if validate_bearer(&headers, &state.bearer_token).is_err() {
    return unauthorized_response();
  }

  match state.backend.reserve(&body.key, &body.version).await {
    Ok(cache_id) => (
      StatusCode::OK,
      Json(serde_json::json!({"cacheId": cache_id})),
    )
      .into_response(),
    Err(e) => error_response(&e),
  }
}

/// PATCH /_apis/artifactcache/caches/:cache_id -- upload chunk.
pub(super) async fn handle_upload_chunk(
  State(state): State<Arc<ServiceState>>,
  headers: HeaderMap,
  Path(cache_id): Path<u64>,
  body: Bytes,
) -> impl IntoResponse {
  if validate_bearer(&headers, &state.bearer_token).is_err() {
    return unauthorized_response();
  }

  let offset = parse_content_range_start(&headers);

  match state
    .backend
    .upload_chunk(cache_id, offset, body.to_vec())
    .await
  {
    Ok(()) => (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response(),
    Err(e) => error_response(&e),
  }
}

#[derive(Deserialize)]
pub(super) struct FinalizeBody {
  size: u64,
}

/// POST /_apis/artifactcache/caches/:cache_id -- finalize.
pub(super) async fn handle_finalize(
  State(state): State<Arc<ServiceState>>,
  headers: HeaderMap,
  Path(cache_id): Path<u64>,
  Json(body): Json<FinalizeBody>,
) -> impl IntoResponse {
  if validate_bearer(&headers, &state.bearer_token).is_err() {
    return unauthorized_response();
  }

  match state.backend.finalize(cache_id, body.size).await {
    Ok(()) => (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response(),
    Err(e) => error_response(&e),
  }
}

/// GET /_apis/artifactcache/download/:cache_id -- download cache content.
pub(super) async fn handle_download(
  State(state): State<Arc<ServiceState>>,
  headers: HeaderMap,
  Path(cache_id): Path<u64>,
) -> impl IntoResponse {
  if validate_bearer(&headers, &state.bearer_token).is_err() {
    return (StatusCode::UNAUTHORIZED, axum::body::Body::empty()).into_response();
  }

  match state.backend.download(cache_id).await {
    Ok(data) => (StatusCode::OK, axum::body::Body::from(data)).into_response(),
    Err(_) => (StatusCode::NOT_FOUND, axum::body::Body::empty()).into_response(),
  }
}
