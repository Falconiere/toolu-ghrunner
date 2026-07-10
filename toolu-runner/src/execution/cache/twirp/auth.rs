//! Bearer authentication and host resolution for the Twirp cache handlers.
//!
//! Accelerated mode forwards the real GitHub runtime token, so local auth is a
//! constant-time comparison against that same token — no JWT parsing. Signed
//! blob URLs are built from the request's `Host` header so the client reaches
//! the exact endpoint it called.

use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};

/// True if the request carries `Authorization: Bearer <bearer>`, matched in
/// constant time.
pub fn check_bearer(headers: &HeaderMap, bearer: &str) -> bool {
  let Some(value) = headers
    .get(header::AUTHORIZATION)
    .and_then(|v| v.to_str().ok())
  else {
    return false;
  };
  let Some(token) = value.strip_prefix("Bearer ") else {
    return false;
  };
  constant_time_eq(token.as_bytes(), bearer.as_bytes())
}

/// The request `Host` header, or `127.0.0.1` when absent, for signed URLs.
pub fn host_from(headers: &HeaderMap) -> String {
  headers
    .get(header::HOST)
    .and_then(|v| v.to_str().ok())
    .unwrap_or("127.0.0.1")
    .to_owned()
}

/// A bare `401 Unauthorized` for a missing or wrong bearer token.
pub fn unauthorized() -> Response {
  StatusCode::UNAUTHORIZED.into_response()
}

/// Constant-time byte-slice equality: accumulate the difference of every byte
/// pair rather than short-circuiting on the first mismatch.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
  if a.len() != b.len() {
    return false;
  }
  let mut diff = 0u8;
  for (x, y) in a.iter().zip(b.iter()) {
    diff |= x ^ y;
  }
  diff == 0
}
