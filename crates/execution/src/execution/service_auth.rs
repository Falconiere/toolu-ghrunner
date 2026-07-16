//! Bearer-token validation shared across local HTTP services.

use axum::http::{HeaderMap, StatusCode};

/// Validate the Authorization header against the expected bearer token.
///
/// Shared across OIDC, artifact, and cache Axum services.
///
/// # Errors
///
/// Returns `StatusCode::UNAUTHORIZED` if the header is missing, malformed, or wrong.
pub fn validate_bearer(headers: &HeaderMap, expected: &str) -> Result<(), StatusCode> {
  let auth_header = headers
    .get("Authorization")
    .and_then(|v| v.to_str().ok())
    .ok_or(StatusCode::UNAUTHORIZED)?;

  if !auth_header.starts_with("Bearer ") {
    return Err(StatusCode::UNAUTHORIZED);
  }

  let token = &auth_header["Bearer ".len()..];
  if !constant_time_eq(token.as_bytes(), expected.as_bytes()) {
    return Err(StatusCode::UNAUTHORIZED);
  }

  Ok(())
}

/// Fixed-width digest equality: hash both inputs to a 32-byte BLAKE3 digest,
/// then compare the digests byte-by-byte without short-circuiting.
///
/// Mirrors `cache::twirp::auth`'s constant-time compare (BLAKE3 rather than
/// SHA-256 only because `blake3` is already an `execution` dependency and
/// `sha2` is not). The compare loop always runs over exactly 32 bytes, so a
/// plain `token != expected` timing oracle on the secret-bearing comparison is
/// removed. The server-fixed `expected` hashes to the same constant cost every
/// request; the attacker-supplied `token` only reveals its own known length.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
  let da = blake3::hash(a);
  let db = blake3::hash(b);
  let mut diff = 0u8;
  for (x, y) in da.as_bytes().iter().zip(db.as_bytes().iter()) {
    diff |= x ^ y;
  }
  diff == 0
}
