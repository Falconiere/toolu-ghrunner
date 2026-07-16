//! Bearer-token validation for the local OIDC/artifact/cache services.
//!
//! Closes a pre-push review gap: the constant-time `validate_bearer` path
//! (backed by `constant_time_eq`) shipped with zero tests. Drives it with a
//! REAL `axum::http::HeaderMap` (no mocks) and asserts the FULL identity of
//! every rejection — `StatusCode::UNAUTHORIZED`, not a loose is-err check.

use axum::http::{HeaderMap, HeaderValue, StatusCode};
use execution::execution::service_auth::validate_bearer;

const EXPECTED: &str = "s3cr3t-runtime-token-abc123";

/// A correct `Bearer <token>` header is accepted.
#[test]
fn correct_bearer_token_is_accepted() {
  let mut headers = HeaderMap::new();
  headers.insert(
    "Authorization",
    HeaderValue::from_str(&format!("Bearer {EXPECTED}")).unwrap(),
  );

  assert_eq!(validate_bearer(&headers, EXPECTED), Ok(()));
}

/// A well-formed `Bearer` header carrying the wrong token is rejected with
/// exactly UNAUTHORIZED (the constant-time compare must still fail closed).
#[test]
fn wrong_token_is_unauthorized() {
  let mut headers = HeaderMap::new();
  headers.insert(
    "Authorization",
    HeaderValue::from_static("Bearer not-the-right-token"),
  );

  assert_eq!(
    validate_bearer(&headers, EXPECTED),
    Err(StatusCode::UNAUTHORIZED)
  );
}

/// A token that shares a prefix with the expected one but differs in length
/// must still be rejected (guards against a truncating/prefix compare).
#[test]
fn prefix_of_expected_token_is_unauthorized() {
  let prefix = &EXPECTED[..EXPECTED.len() - 3];
  let mut headers = HeaderMap::new();
  headers.insert(
    "Authorization",
    HeaderValue::from_str(&format!("Bearer {prefix}")).unwrap(),
  );

  assert_eq!(
    validate_bearer(&headers, EXPECTED),
    Err(StatusCode::UNAUTHORIZED)
  );
}

/// No Authorization header at all → UNAUTHORIZED.
#[test]
fn missing_authorization_header_is_unauthorized() {
  let headers = HeaderMap::new();

  assert_eq!(
    validate_bearer(&headers, EXPECTED),
    Err(StatusCode::UNAUTHORIZED)
  );
}

/// A header present but WITHOUT the `Bearer ` scheme prefix → UNAUTHORIZED,
/// even when the raw value equals the expected token.
#[test]
fn malformed_header_without_bearer_prefix_is_unauthorized() {
  let mut headers = HeaderMap::new();
  // Right token, wrong scheme: `Basic ` / bare value must not authenticate.
  headers.insert("Authorization", HeaderValue::from_str(EXPECTED).unwrap());

  assert_eq!(
    validate_bearer(&headers, EXPECTED),
    Err(StatusCode::UNAUTHORIZED)
  );

  let mut basic = HeaderMap::new();
  basic.insert(
    "Authorization",
    HeaderValue::from_str(&format!("Basic {EXPECTED}")).unwrap(),
  );

  assert_eq!(
    validate_bearer(&basic, EXPECTED),
    Err(StatusCode::UNAUTHORIZED)
  );
}
