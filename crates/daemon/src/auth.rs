//! Bearer token verification for the daemon's HTTP surface.
//!
//! Verifies an `Authorization: Bearer <token>` header value against the
//! token file `config::read_tokens` exposes — the current token, plus an
//! optional previous token accepted during rotation. The token file is
//! re-read fresh on every call, so an operator rewriting it (new token above
//! the old one, then later dropping the old line) takes effect immediately,
//! with no daemon restart and no cache to invalidate.
//!
//! This module is deliberately pure: it takes the raw header value and a
//! path, and returns a typed error the HTTP layer maps to a 401 response. It
//! does not depend on axum, so it can be exercised directly against real
//! token files on disk without standing up a server.

use std::fmt;
use std::path::Path;

use crate::config::{self, ConfigError};

/// The expected `Authorization` scheme; anything else is rejected.
const BEARER_PREFIX: &str = "Bearer ";

/// Why a request's bearer credentials were rejected.
#[derive(Debug)]
pub enum AuthError {
  /// No `Authorization` header was present.
  MissingHeader,
  /// The `Authorization` header was present but did not use the `Bearer`
  /// scheme.
  InvalidScheme,
  /// The header used the `Bearer` scheme but carried an empty token.
  EmptyToken,
  /// The token did not match the current token, nor the previous token
  /// when one is present.
  TokenMismatch,
  /// The token file could not be read to verify against.
  TokenFile(ConfigError),
}

impl fmt::Display for AuthError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::MissingHeader => write!(f, "missing Authorization header"),
      Self::InvalidScheme => write!(f, "Authorization header must use the Bearer scheme"),
      Self::EmptyToken => write!(f, "bearer token is empty"),
      Self::TokenMismatch => write!(f, "bearer token does not match"),
      Self::TokenFile(source) => write!(f, "failed to verify bearer token: {source}"),
    }
  }
}

impl std::error::Error for AuthError {
  fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
    match self {
      Self::TokenFile(source) => Some(source),
      Self::MissingHeader | Self::InvalidScheme | Self::EmptyToken | Self::TokenMismatch => None,
    }
  }
}

/// Verify an `Authorization` header value against the token file at `path`.
///
/// `header` is the raw header value as received (e.g. `"Bearer abc123"`),
/// or `None` when the header was absent. Accepts the token file's current
/// token, and its previous token when the file carries one on line 2 — the
/// mechanism that lets an operator rotate the token without restarting the
/// daemon. The file is re-read on every call via [`config::read_tokens`], so
/// a rewrite is visible immediately.
///
/// # Errors
///
/// Returns [`AuthError::MissingHeader`] when `header` is `None`,
/// [`AuthError::InvalidScheme`] when it does not use the `Bearer` scheme,
/// [`AuthError::EmptyToken`] when the bearer value is empty,
/// [`AuthError::TokenMismatch`] when it matches neither accepted token, or
/// [`AuthError::TokenFile`] when the token file cannot be read.
pub fn verify_bearer(header: Option<&str>, token_file: &Path) -> Result<(), AuthError> {
  let header = header.ok_or(AuthError::MissingHeader)?;
  let token = header
    .strip_prefix(BEARER_PREFIX)
    .ok_or(AuthError::InvalidScheme)?;
  if token.is_empty() {
    return Err(AuthError::EmptyToken);
  }

  let tokens = config::read_tokens(token_file).map_err(AuthError::TokenFile)?;

  let matches_current = constant_time_eq(token.as_bytes(), tokens.current.as_bytes());
  let matches_previous = tokens
    .previous
    .as_deref()
    .is_some_and(|previous| constant_time_eq(token.as_bytes(), previous.as_bytes()));

  if matches_current || matches_previous {
    Ok(())
  } else {
    Err(AuthError::TokenMismatch)
  }
}

/// Compare two byte slices for equality without branching on the content of
/// any single byte, so a mismatch cannot be timed to learn how many leading
/// bytes matched. Bearer tokens are secrets read from an on-disk file; a
/// short-circuiting `==` would let an attacker recover one byte at a time by
/// measuring response latency.
///
/// The length check up front is not a timing leak worth closing: token
/// length is a fixed, effectively public property of the deployment, not
/// part of the secret itself. Only the per-byte comparison that follows must
/// stay branch-free.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
  if a.len() != b.len() {
    return false;
  }
  let mut diff: u8 = 0;
  for (x, y) in a.iter().zip(b.iter()) {
    diff |= x ^ y;
  }
  diff == 0
}

#[cfg(test)]
#[path = "tests/auth.rs"]
mod tests;
