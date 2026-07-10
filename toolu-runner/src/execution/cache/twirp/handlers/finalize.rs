//! `FinalizeCacheEntryUpload`: chunk the staged bytes into the CAS and index
//! the entry. The client sends `(key, size_bytes, version)` but not the blob
//! token, so the pending upload is resolved by its cache coordinates. A size
//! mismatch, an unknown upload, or an ingest error all reject with `ok:false`
//! and never index a lying entry; the job rebuilds.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use shared::RunnerError;

use super::super::TwirpState;
use super::super::auth::{check_bearer, unauthorized};
use super::super::types::{FinalizeRequest, FinalizeResponse};
use crate::execution::cache::cas::{IndexEntry, entry_id_for};

/// Handle `POST .../FinalizeCacheEntryUpload`: ingest + index, or `ok:false`.
pub async fn finalize_cache_entry_upload(
  State(state): State<Arc<TwirpState>>,
  headers: HeaderMap,
  Json(req): Json<FinalizeRequest>,
) -> Response {
  if !check_bearer(&headers, &state.bearer) {
    return unauthorized();
  }
  match finalize(&state, &req).await {
    Ok(Some(entry_id)) => Json(FinalizeResponse::ok(entry_id)).into_response(),
    Ok(None) => Json(FinalizeResponse::failed()).into_response(),
    Err(e) => {
      tracing::warn!(error = %e, "FinalizeCacheEntryUpload failed");
      Json(FinalizeResponse::failed()).into_response()
    },
  }
}

/// Ingest the staged upload and index it; `Ok(None)` rejects with `ok:false`.
async fn finalize(
  state: &TwirpState,
  req: &FinalizeRequest,
) -> Result<Option<String>, RunnerError> {
  let size_bytes = req
    .size_bytes
    .parse::<u64>()
    .map_err(|e| RunnerError::Cache(format!("bad size_bytes: {e}")))?;
  let Some((_token, staging)) =
    state
      .registry
      .take_pending_upload(&state.scopes.write, &req.key, &req.version)
  else {
    return Ok(None);
  };
  let manifest = state.store.ingest(&staging).await?;
  let _ = tokio::fs::remove_file(&staging).await;
  if manifest.total_size != size_bytes {
    return Ok(None);
  }
  let mid = state.store.put_manifest(&manifest).await?;
  let entry = IndexEntry {
    key: req.key.clone(),
    manifest: mid.clone(),
    size_bytes,
    created_at: Utc::now(),
  };
  state
    .index
    .insert(&state.scopes.write, &req.version, &entry)?;
  Ok(Some(entry_id_for(&mid).to_string()))
}
