//! Bearer authentication and host resolution for the Twirp cache handlers.
//!
//! Accelerated mode forwards the real GitHub runtime token, so local auth is a
//! constant-time comparison against that same token — no JWT parsing. Signed
//! blob URLs are built from the request's `Host` header so the client reaches
//! the exact endpoint it called.

use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};

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

/// Fixed-width digest equality: hash both inputs to a 32-byte SHA-256 digest,
/// then compare the digests byte-by-byte without short-circuiting.
///
/// The compare loop always runs over exactly 32 bytes regardless of input, so
/// it leaks nothing about either length — this removes the old early length
/// return and min-length loop, which revealed the expected token's length
/// through timing. It is not *perfectly* constant-time overall: SHA-256 costs
/// one block per 64 input bytes, so hashing time varies coarsely with length.
/// But the expected token `b` is fixed server-side, so its hashing cost is the
/// same constant offset on every request and carries no per-request signal;
/// the attacker-supplied `a` only reveals its own already-known length. The
/// secret-bearing comparison itself is the genuinely constant-time part.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
  let da = Sha256::digest(a);
  let db = Sha256::digest(b);
  let mut diff = 0u8;
  for (x, y) in da.iter().zip(db.iter()) {
    diff |= x ^ y;
  }
  diff == 0
}
