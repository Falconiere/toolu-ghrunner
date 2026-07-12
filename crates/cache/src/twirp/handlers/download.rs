//! `GetCacheEntryDownloadURL`: resolve `(key, restore_keys, version)` through
//! the read ladder to a signed download URL, or a bare `{"ok":false}` miss.
//!
//! A missing manifest or a missing chunk (partial GC, torn mirror) resolves to
//! a miss, never a 500: a miss makes the job rebuild, a 500 fails it.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use shared::RunnerError;

use super::super::TwirpState;
use super::super::auth::{check_bearer, host_from, unauthorized};
use super::super::types::{DownloadRequest, DownloadResponse};
use crate::cas::{CasStore, Manifest};

/// Handle `POST .../GetCacheEntryDownloadURL`: a signed URL or a bare miss.
pub async fn get_cache_entry_download_url(
  State(state): State<Arc<TwirpState>>,
  headers: HeaderMap,
  Json(req): Json<DownloadRequest>,
) -> Response {
  if !check_bearer(&headers, &state.bearer) {
    return unauthorized();
  }
  let host = host_from(&headers);
  Json(resolve(&state, &req, &host).await).into_response()
}

/// Resolve a hit into a download response, or a miss on absence or any error.
async fn resolve(state: &TwirpState, req: &DownloadRequest, host: &str) -> DownloadResponse {
  match lookup_url(state, req, host).await {
    Ok(Some(resp)) => resp,
    Ok(None) => DownloadResponse::miss(),
    Err(e) => {
      tracing::warn!(error = %e, "GetCacheEntryDownloadURL failed");
      DownloadResponse::miss()
    },
  }
}

/// Look up the entry, verify every chunk exists, and mint a download token.
async fn lookup_url(
  state: &TwirpState,
  req: &DownloadRequest,
  host: &str,
) -> Result<Option<DownloadResponse>, RunnerError> {
  let Some((matched_key, entry)) = state.index.lookup(
    &state.scopes.read_ladder,
    &req.version,
    &req.key,
    &req.restore_keys,
  )?
  else {
    return Ok(None);
  };
  let Ok(manifest) = state.store.get_manifest(&entry.manifest).await else {
    return Ok(None);
  };
  if !all_chunks_present(&state.store, &manifest).await {
    return Ok(None);
  }
  let token = state.registry.mint_download(manifest, state.download_ttl);
  let url = format!("http://{host}/_toolu/blob/{token}");
  Ok(Some(DownloadResponse::hit(url, matched_key)))
}

/// True if every chunk the manifest references still exists on disk.
async fn all_chunks_present(store: &CasStore, manifest: &Manifest) -> bool {
  for chunk in &manifest.chunks {
    if !store.has_chunk(&chunk.id).await {
      return false;
    }
  }
  true
}
