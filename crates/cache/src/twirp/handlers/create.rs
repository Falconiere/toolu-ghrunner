//! `CreateCacheEntry`: mint a signed upload URL for a new `(scope, key,
//! version)`, or refuse — a protected-scope write from an untrusted job, or a
//! duplicate entry. The literal `cache write denied:` prefix on a denial is
//! load-bearing: the toolkit turns it into a soft `CacheWriteDeniedError` that
//! does not fail the job.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use shared::RunnerError;
use uuid::Uuid;

use super::super::TwirpState;
use super::super::auth::{check_bearer, host_from, unauthorized};
use super::super::types::{CreateRequest, CreateResponse};
use crate::trust::write_allowed;

/// Handle `POST .../CreateCacheEntry`: deny, duplicate, or mint an upload URL.
pub async fn create_cache_entry(
  State(state): State<Arc<TwirpState>>,
  headers: HeaderMap,
  Json(req): Json<CreateRequest>,
) -> Response {
  if !check_bearer(&headers, &state.bearer) {
    return unauthorized();
  }
  let host = host_from(&headers);
  match build(&state, &req, &host) {
    Ok(resp) => Json(resp).into_response(),
    Err(e) => {
      tracing::warn!(error = %e, "CreateCacheEntry failed");
      Json(CreateResponse::refused(format!("cache create failed: {e}"))).into_response()
    },
  }
}

/// Decide the outcome: denied protected write, duplicate entry, or a fresh URL.
fn build(
  state: &TwirpState,
  req: &CreateRequest,
  host: &str,
) -> Result<CreateResponse, RunnerError> {
  let write = &state.scopes.write;
  if !write_allowed(write, state.trust, &state.protected) {
    return Ok(CreateResponse::refused(format!(
      "cache write denied: branch '{write}' may not write a protected scope"
    )));
  }
  if entry_exists(state, req)? {
    return Ok(CreateResponse::refused(
      "cache entry already exists".to_owned(),
    ));
  }
  Ok(mint(state, req, host))
}

/// True if an exact `(write scope, version, key)` entry is already indexed.
fn entry_exists(state: &TwirpState, req: &CreateRequest) -> Result<bool, RunnerError> {
  let ladder = std::slice::from_ref(&state.scopes.write);
  let hit = state.index.lookup(ladder, &req.version, &req.key, &[])?;
  Ok(hit.is_some())
}

/// Mint an upload token staging to `cas/staging/<uuid>`; build its signed URL.
fn mint(state: &TwirpState, req: &CreateRequest, host: &str) -> CreateResponse {
  let staging = state.staging_root.join(Uuid::new_v4().to_string());
  let token = state.registry.mint_upload(
    staging,
    state.scopes.write.clone(),
    req.key.clone(),
    req.version.clone(),
    state.upload_ttl,
  );
  CreateResponse::ok_upload(format!("http://{host}/_toolu/blob/{token}"))
}
