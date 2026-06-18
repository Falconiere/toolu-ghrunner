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
  if token != expected {
    return Err(StatusCode::UNAUTHORIZED);
  }

  Ok(())
}
