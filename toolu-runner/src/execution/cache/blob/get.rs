//! Download side of the blob endpoint: `HEAD` (Get Blob Properties) returns the
//! total size as `Content-Length`; `GET` streams the whole object (`200`) or a
//! byte range (`206` + `Content-Range`). Every chunk is BLAKE3-verified as it
//! streams, and a `LeaseSet` lease is held for the response so GC cannot delete
//! a chunk mid-read.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::Response;
use futures_util::StreamExt;
use shared::RunnerError;

use super::token::{BlobRegistry, BlobTarget};
use super::{BlobState, add_required_headers, error_500, forbidden, hv};
use crate::execution::cache::cas::{ChunkId, Manifest};

/// `HEAD /_toolu/blob/{token}` — Get Blob Properties.
pub(super) async fn head(
  State(state): State<Arc<BlobState>>,
  Path(token): Path<String>,
) -> Response {
  match build_head(&state, &token) {
    Ok(resp) => resp,
    Err(e) => error_500(&e),
  }
}

/// `GET /_toolu/blob/{token}` — full body (`200`) or ranged (`206`).
pub(super) async fn get(
  State(state): State<Arc<BlobState>>,
  Path(token): Path<String>,
  headers: HeaderMap,
) -> Response {
  match build_get(&state, &token, &headers) {
    Ok(resp) => resp,
    Err(e) => error_500(&e),
  }
}

/// Build the HEAD response, or a `403` for a missing/expired/non-download token.
fn build_head(state: &BlobState, token: &str) -> Result<Response, RunnerError> {
  let Some(manifest) = download_manifest(&state.registry, token) else {
    return forbidden();
  };
  let mut resp = Response::new(Body::empty());
  resp.headers_mut().insert(
    header::CONTENT_LENGTH,
    hv(&manifest.total_size.to_string())?,
  );
  add_required_headers(&mut resp, token)?;
  Ok(resp)
}

/// Build the GET response, streaming the requested range under a read lease.
fn build_get(state: &BlobState, token: &str, headers: &HeaderMap) -> Result<Response, RunnerError> {
  let Some(manifest) = download_manifest(&state.registry, token) else {
    return forbidden();
  };
  let total = manifest.total_size;
  let (status, offset, len, content_range) = match parse_range(headers, total)? {
    Some((start, end)) => {
      let len = end
        .checked_sub(start)
        .and_then(|d| d.checked_add(1))
        .ok_or_else(|| RunnerError::Cache("range length overflow".into()))?;
      let cr = format!("bytes {start}-{end}/{total}");
      (StatusCode::PARTIAL_CONTENT, start, len, Some(cr))
    },
    None => (StatusCode::OK, 0u64, total, None),
  };
  let body = leased_body(state, &manifest, offset, len);
  finish_get(status, len, content_range, token, body)
}

/// Assemble the final GET response with `Content-Length`, `Content-Range`, and
/// the required Azure headers.
fn finish_get(
  status: StatusCode,
  len: u64,
  content_range: Option<String>,
  token: &str,
  body: Body,
) -> Result<Response, RunnerError> {
  let mut resp = Response::new(body);
  *resp.status_mut() = status;
  resp
    .headers_mut()
    .insert(header::CONTENT_LENGTH, hv(&len.to_string())?);
  if let Some(cr) = content_range {
    resp.headers_mut().insert(header::CONTENT_RANGE, hv(&cr)?);
  }
  add_required_headers(&mut resp, token)?;
  Ok(resp)
}

/// A streaming body over `[offset, offset+len)` that holds a chunk lease for its
/// whole lifetime; each chunk is BLAKE3-verified, so corruption aborts the body.
fn leased_body(state: &BlobState, manifest: &Manifest, offset: u64, len: u64) -> Body {
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

/// The manifest a download token points at, or `None` for a missing/expired or
/// upload token.
fn download_manifest(registry: &BlobRegistry, token: &str) -> Option<Manifest> {
  match registry.get(token)? {
    BlobTarget::Download { manifest } => Some(manifest),
    BlobTarget::Upload { .. } => None,
  }
}

/// Parse `Range: bytes=a-b` into an inclusive `(start, end)` clamped to `total`.
///
/// A missing header is `Ok(None)`. An open end (`bytes=a-`) runs to the last
/// byte. A malformed header or `start > end` is `RunnerError::Cache`.
fn parse_range(headers: &HeaderMap, total: u64) -> Result<Option<(u64, u64)>, RunnerError> {
  let Some(raw) = headers.get(header::RANGE).and_then(|v| v.to_str().ok()) else {
    return Ok(None);
  };
  let spec = raw
    .strip_prefix("bytes=")
    .ok_or_else(|| RunnerError::Cache(format!("unsupported range: {raw}")))?;
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
