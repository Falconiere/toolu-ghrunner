//! The five v1 REST handlers, backed by the CAS store + index.
//!
//! Route shapes match `actions/cache`'s legacy `/_apis/artifactcache/*` API
//! verbatim; only the storage is new. Every route except `download`
//! bearer-checks against `V1State::bearer` in constant time; `download` is an
//! unauthenticated token capability (real clients send no header). A protected
//! write from an untrusted job is `403`. A lookup miss is `204`, a chunk PATCH
//! without a parseable `Content-Range` is `400 {"ok":false}` (never written), a
//! finalize size mismatch is `400 {"ok":false}` (never indexed), and a download
//! honours `Range` with `206` + `Content-Range` under a read lease (a malformed
//! or unsatisfiable `Range` is `416`).

use std::sync::Arc;

use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::json;
use shared::RunnerError;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};

use super::{Reservation, V1State};
use crate::cas::{ChunkId, IndexEntry, Manifest};
use crate::trust::write_allowed;
use crate::twirp::auth::{check_bearer, host_from};

/// `GET /_apis/artifactcache/cache` query: comma-joined keys and exact version.
#[derive(Deserialize)]
pub struct LookupParams {
  /// Primary key, then restore-key fallbacks, comma-separated.
  keys: String,
  /// Exact cache version.
  version: String,
}

/// `GET /_apis/artifactcache/cache` — a hit's archive location, or `204` on miss.
pub async fn lookup(
  State(state): State<Arc<V1State>>,
  headers: HeaderMap,
  Query(params): Query<LookupParams>,
) -> Response {
  if !check_bearer(&headers, &state.bearer) {
    return StatusCode::UNAUTHORIZED.into_response();
  }
  let host = host_from(&headers);
  match resolve_lookup(&state, &params, &host).await {
    Ok(Some(resp)) => resp,
    Ok(None) => StatusCode::NO_CONTENT.into_response(),
    Err(e) => {
      tracing::warn!(error = %e, "cache v1 lookup failed");
      StatusCode::NO_CONTENT.into_response()
    },
  }
}

/// Resolve `keys`/`version` through the read ladder to a hit response, or `None`.
///
/// # Errors
/// `RunnerError` only if the index cannot be read; a genuine miss is `Ok(None)`.
async fn resolve_lookup(
  state: &V1State,
  params: &LookupParams,
  host: &str,
) -> Result<Option<Response>, RunnerError> {
  let keys: Vec<String> = params
    .keys
    .split(',')
    .map(|k| k.trim().to_owned())
    .filter(|k| !k.is_empty())
    .collect();
  let Some((primary, restore)) = keys.split_first() else {
    return Ok(None);
  };
  let Some((matched, entry)) =
    state
      .index
      .lookup(&state.scopes.read_ladder, &params.version, primary, restore)?
  else {
    return Ok(None);
  };
  let Ok(manifest) = state.store.get_manifest(&entry.manifest).await else {
    return Ok(None);
  };
  if !all_chunks_present(state, &manifest).await {
    return Ok(None);
  }
  let token = state.register_download(manifest);
  let url = format!("http://{host}/_apis/artifactcache/download/{token}");
  Ok(Some(hit_response(&matched, &state.scopes.write, &url)))
}

/// True if every chunk the manifest references still exists on disk.
async fn all_chunks_present(state: &V1State, manifest: &Manifest) -> bool {
  for chunk in &manifest.chunks {
    if !state.store.has_chunk(&chunk.id).await {
      return false;
    }
  }
  true
}

/// The `200` hit body: matched key, scope, and the archive download URL.
fn hit_response(matched_key: &str, scope: &str, archive_url: &str) -> Response {
  (
    StatusCode::OK,
    Json(json!({
      "cacheKey": matched_key,
      "scope": scope,
      "archiveLocation": archive_url,
    })),
  )
    .into_response()
}

/// `POST /_apis/artifactcache/caches` body: the key + version to reserve.
#[derive(Deserialize)]
pub struct ReserveBody {
  /// Client-supplied, opaque cache key.
  key: String,
  /// Client-supplied, opaque cache version.
  version: String,
}

/// `POST /_apis/artifactcache/caches` — allocate a cache id + staging file.
pub async fn reserve(
  State(state): State<Arc<V1State>>,
  headers: HeaderMap,
  Json(body): Json<ReserveBody>,
) -> Response {
  if !check_bearer(&headers, &state.bearer) {
    return StatusCode::UNAUTHORIZED.into_response();
  }
  if !write_allowed(&state.scopes.write, state.trust, &state.protected) {
    return StatusCode::FORBIDDEN.into_response();
  }
  match do_reserve(&state, &body).await {
    Ok(id) => (StatusCode::OK, Json(json!({ "cacheId": id }))).into_response(),
    Err(e) => internal_error(&e),
  }
}

/// Create the staging file and record the reservation; returns its cache id.
///
/// # Errors
/// `RunnerError::Io` if the staging directory or file cannot be created.
async fn do_reserve(state: &V1State, body: &ReserveBody) -> Result<u64, RunnerError> {
  tokio::fs::create_dir_all(&state.staging_root)
    .await
    .map_err(RunnerError::Io)?;
  let id = state.alloc_id();
  let staging = state.staging_root.join(id.to_string());
  tokio::fs::File::create(&staging)
    .await
    .map_err(RunnerError::Io)?;
  state.insert_reservation(
    id,
    Reservation {
      key: body.key.clone(),
      version: body.version.clone(),
      staging,
    },
  );
  Ok(id)
}

/// `PATCH /_apis/artifactcache/caches/{cache_id}` — write one `Content-Range` chunk.
///
/// An absent or malformed `Content-Range` is a `400`: guessing offset `0` would
/// silently overwrite the start of the staging file. Real `@actions/cache` /
/// buildx clients always send `bytes START-END/*` on PATCH.
pub async fn upload_chunk(
  State(state): State<Arc<V1State>>,
  headers: HeaderMap,
  Path(cache_id): Path<u64>,
  body: Bytes,
) -> Response {
  if !check_bearer(&headers, &state.bearer) {
    return StatusCode::UNAUTHORIZED.into_response();
  }
  let Some(offset) = content_range_start(&headers) else {
    return rejected();
  };
  match write_chunk(&state, cache_id, offset, &body).await {
    Ok(()) => ok_true(),
    Err(e) => internal_error(&e),
  }
}

/// Parse the start offset from a `Content-Range: bytes START-END/TOTAL` header.
///
/// `None` when the header is absent or malformed — the caller must reject the
/// chunk rather than fall back to offset `0` and corrupt the staging file.
fn content_range_start(headers: &HeaderMap) -> Option<u64> {
  let range = headers.get(header::CONTENT_RANGE)?.to_str().ok()?;
  let after_bytes = range.strip_prefix("bytes ")?;
  let (start, _) = after_bytes.split_once('-')?;
  start.trim().parse().ok()
}

/// Seek to `offset` in the reservation's staging file and write `data`.
///
/// # Errors
/// `RunnerError::Cache` for an unknown reservation, `RunnerError::Io` on write.
async fn write_chunk(
  state: &V1State,
  cache_id: u64,
  offset: u64,
  data: &[u8],
) -> Result<(), RunnerError> {
  let Some(staging) = state.reservation_staging(cache_id) else {
    return Err(RunnerError::Cache(format!(
      "no reservation for cache id {cache_id}"
    )));
  };
  let mut file = tokio::fs::OpenOptions::new()
    .create(true)
    .truncate(false)
    .write(true)
    .open(&staging)
    .await
    .map_err(RunnerError::Io)?;
  file
    .seek(std::io::SeekFrom::Start(offset))
    .await
    .map_err(RunnerError::Io)?;
  file.write_all(data).await.map_err(RunnerError::Io)?;
  Ok(())
}

/// `POST /_apis/artifactcache/caches/{cache_id}` body: the finalized total size.
#[derive(Deserialize)]
pub struct FinalizeBody {
  /// Total assembled archive size, verified against the ingested manifest.
  size: u64,
}

/// `POST /_apis/artifactcache/caches/{cache_id}` — ingest + index, or reject.
pub async fn finalize(
  State(state): State<Arc<V1State>>,
  headers: HeaderMap,
  Path(cache_id): Path<u64>,
  Json(body): Json<FinalizeBody>,
) -> Response {
  if !check_bearer(&headers, &state.bearer) {
    return StatusCode::UNAUTHORIZED.into_response();
  }
  if !write_allowed(&state.scopes.write, state.trust, &state.protected) {
    return StatusCode::FORBIDDEN.into_response();
  }
  match do_finalize(&state, cache_id, body.size).await {
    Ok(true) => ok_true(),
    Ok(false) => rejected(),
    Err(e) => internal_error(&e),
  }
}

/// Ingest the staged bytes and index them; `Ok(false)` rejects without indexing.
///
/// # Errors
/// `RunnerError` if ingest, manifest persistence, or the index write fails.
async fn do_finalize(state: &V1State, cache_id: u64, size: u64) -> Result<bool, RunnerError> {
  let Some(reservation) = state.take_reservation(cache_id) else {
    return Ok(false);
  };
  let manifest = state.store.ingest(&reservation.staging).await?;
  let _ = tokio::fs::remove_file(&reservation.staging).await;
  if manifest.total_size != size {
    return Ok(false);
  }
  let mid = state.store.put_manifest(&manifest).await?;
  let entry = IndexEntry {
    key: reservation.key,
    manifest: mid,
    size_bytes: size,
    created_at: Utc::now(),
  };
  state
    .index
    .insert(&state.scopes.write, &reservation.version, &entry)?;
  Ok(true)
}

/// `GET /_apis/artifactcache/download/{token}` — streamed, range-aware read.
///
/// Intentionally unauthenticated: real `@actions/cache` / buildx clients GET a
/// v1 archive URL with no `Authorization` header. The unguessable token in the
/// path is the capability (see [`crate::mint_capability_token`]).
pub async fn download(
  State(state): State<Arc<V1State>>,
  headers: HeaderMap,
  Path(token): Path<String>,
) -> Response {
  match stream_download(&state, &token, &headers) {
    Ok(resp) => resp,
    Err(e) => internal_error(&e),
  }
}

/// Build the download response: full body (`200`), a ranged slice (`206`), or a
/// `416` for a `Range` header we cannot satisfy.
///
/// # Errors
/// `RunnerError::Cache` if a response header cannot be constructed.
fn stream_download(
  state: &V1State,
  token: &str,
  headers: &HeaderMap,
) -> Result<Response, RunnerError> {
  let Some(manifest) = state.download_manifest(token) else {
    return Ok(StatusCode::NOT_FOUND.into_response());
  };
  let total = manifest.total_size;
  let (status, offset, len, content_range) = match plan_range(headers, total) {
    Ok(plan) => plan,
    Err(e) => return range_not_satisfiable(total, &e),
  };
  let body = leased_body(state, &manifest, offset, len);
  finish_download(status, len, content_range, body)
}

/// The `416 Range Not Satisfiable` reply for a malformed or unsatisfiable
/// `Range`, carrying the `Content-Range: bytes */{total}` marker (RFC 9110).
///
/// # Errors
/// `RunnerError::Cache` if the `Content-Range` header cannot be constructed.
fn range_not_satisfiable(total: u64, e: &RunnerError) -> Result<Response, RunnerError> {
  tracing::debug!(error = %e, "rejecting unsatisfiable range");
  let mut resp = (
    StatusCode::RANGE_NOT_SATISFIABLE,
    Json(json!({ "error": e.to_string() })),
  )
    .into_response();
  resp.headers_mut().insert(
    header::CONTENT_RANGE,
    header_value(&format!("bytes */{total}"))?,
  );
  Ok(resp)
}

/// Decide the status, byte window, and `Content-Range` for a download.
///
/// # Errors
/// `RunnerError::Cache` on a malformed `Range` header or a length overflow.
fn plan_range(
  headers: &HeaderMap,
  total: u64,
) -> Result<(StatusCode, u64, u64, Option<String>), RunnerError> {
  match parse_range(headers, total)? {
    Some((start, end)) => {
      let len = end
        .checked_sub(start)
        .and_then(|d| d.checked_add(1))
        .ok_or_else(|| RunnerError::Cache("range length overflow".into()))?;
      let content_range = format!("bytes {start}-{end}/{total}");
      Ok((StatusCode::PARTIAL_CONTENT, start, len, Some(content_range)))
    },
    None => Ok((StatusCode::OK, 0, total, None)),
  }
}

/// A streaming body over `[offset, offset+len)` holding a chunk lease for its life.
fn leased_body(state: &V1State, manifest: &Manifest, offset: u64, len: u64) -> Body {
  let ids: Vec<ChunkId> = manifest.chunks.iter().map(|c| c.id.clone()).collect();
  let guard = state.leases.acquire(&ids);
  let stream = state
    .store
    .read_range(manifest, offset, len)
    .map(move |item| {
      // `guard` is owned by this closure, so the lease outlives the stream.
      let _held = &guard;
      item
    });
  Body::from_stream(stream)
}

/// Assemble the final download response with `Content-Length` / `Content-Range`.
///
/// # Errors
/// `RunnerError::Cache` if a header value cannot be constructed.
fn finish_download(
  status: StatusCode,
  len: u64,
  content_range: Option<String>,
  body: Body,
) -> Result<Response, RunnerError> {
  let mut resp = Response::new(body);
  *resp.status_mut() = status;
  resp
    .headers_mut()
    .insert(header::CONTENT_LENGTH, header_value(&len.to_string())?);
  if let Some(cr) = content_range {
    resp
      .headers_mut()
      .insert(header::CONTENT_RANGE, header_value(&cr)?);
  }
  resp
    .headers_mut()
    .insert(header::ACCEPT_RANGES, header_value("bytes")?);
  Ok(resp)
}

/// Parse `Range: bytes=a-b` into an inclusive `(start, end)` clamped to `total`.
///
/// Single `bytes` ranges only — our clients (BuildKit, `@actions/cache`) never
/// send multi-range, so `bytes=a-b, c-d` is rejected rather than half-parsed.
///
/// # Errors
/// `RunnerError::Cache` on a malformed header, a multi-range, or `start > end`;
/// the caller maps this to `416`.
fn parse_range(headers: &HeaderMap, total: u64) -> Result<Option<(u64, u64)>, RunnerError> {
  let Some(raw) = headers.get(header::RANGE).and_then(|v| v.to_str().ok()) else {
    return Ok(None);
  };
  let spec = raw
    .strip_prefix("bytes=")
    .ok_or_else(|| RunnerError::Cache(format!("unsupported range: {raw}")))?;
  if spec.contains(',') {
    return Err(RunnerError::Cache(format!(
      "multi-range unsupported: {raw}"
    )));
  }
  let (start_str, end_str) = spec
    .split_once('-')
    .ok_or_else(|| RunnerError::Cache(format!("malformed range: {raw}")))?;
  let start: u64 = start_str
    .trim()
    .parse()
    .map_err(|e| RunnerError::Cache(format!("bad range start: {e}")))?;
  let last = total.saturating_sub(1);
  let end: u64 = if end_str.trim().is_empty() {
    last
  } else {
    end_str
      .trim()
      .parse::<u64>()
      .map_err(|e| RunnerError::Cache(format!("bad range end: {e}")))?
      .min(last)
  };
  if start > end {
    return Err(RunnerError::Cache(format!(
      "range start {start} > end {end}"
    )));
  }
  Ok(Some((start, end)))
}

/// Build a `HeaderValue` from an ASCII string.
///
/// # Errors
/// `RunnerError::Cache` if `s` is not a valid header value.
fn header_value(s: &str) -> Result<HeaderValue, RunnerError> {
  HeaderValue::from_str(s).map_err(|e| RunnerError::Cache(format!("bad header value: {e}")))
}

/// The `200 {"ok":true}` success body shared by chunk upload and finalize.
fn ok_true() -> Response {
  (StatusCode::OK, Json(json!({ "ok": true }))).into_response()
}

/// The `400 {"ok":false}` rejection for an unknown reservation, a size
/// mismatch, or a chunk PATCH without a parseable `Content-Range`.
fn rejected() -> Response {
  (StatusCode::BAD_REQUEST, Json(json!({ "ok": false }))).into_response()
}

/// A `500` JSON error wrapping a genuine internal failure.
fn internal_error(e: &RunnerError) -> Response {
  (
    StatusCode::INTERNAL_SERVER_ERROR,
    Json(json!({ "error": e.to_string() })),
  )
    .into_response()
}
