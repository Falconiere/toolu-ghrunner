//! Tests for bearer auth verification: real token files on disk, exercised
//! through the same `read_tokens` path production wires through `Config`. No
//! mock filesystem and no patched reader — rotation is proven by rewriting a
//! real file between two calls.

use std::fs;
use std::path::{Path, PathBuf};

use super::{AuthError, verify_bearer};

/// Write a token file with the given contents, returning its path.
fn write_token_file(dir: &Path, contents: &str) -> PathBuf {
  let path = dir.join("token");
  fs::write(&path, contents).expect("write token file");
  path
}

#[test]
fn current_token_is_accepted() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = write_token_file(dir.path(), "current-tok\n");

  assert!(verify_bearer(Some("Bearer current-tok"), &path).is_ok());
}

#[test]
fn previous_token_is_accepted_when_line_two_is_present() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = write_token_file(dir.path(), "current-tok\nprevious-tok\n");

  assert!(verify_bearer(Some("Bearer previous-tok"), &path).is_ok());
}

#[test]
fn previous_token_is_rejected_once_the_file_drops_to_one_line() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = write_token_file(dir.path(), "current-tok\nprevious-tok\n");
  assert!(verify_bearer(Some("Bearer previous-tok"), &path).is_ok());

  fs::write(&path, "current-tok\n").expect("rewrite token file without the previous line");

  let result = verify_bearer(Some("Bearer previous-tok"), &path);
  assert!(matches!(result, Err(AuthError::TokenMismatch)));
}

#[test]
fn rotation_is_visible_without_restart_or_reconstruction() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = write_token_file(dir.path(), "old-current\n");

  assert!(verify_bearer(Some("Bearer old-current"), &path).is_ok());
  assert!(verify_bearer(Some("Bearer new-current"), &path).is_err());

  // Rewrite in place — same path, no new handle or config constructed — and
  // verify again through the identical `verify_bearer` call.
  fs::write(&path, "new-current\nold-current\n").expect("rewrite token file for rotation");

  assert!(verify_bearer(Some("Bearer new-current"), &path).is_ok());
  assert!(verify_bearer(Some("Bearer old-current"), &path).is_ok());
}

#[test]
fn missing_header_is_rejected() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = write_token_file(dir.path(), "current-tok\n");

  let result = verify_bearer(None, &path);
  assert!(matches!(result, Err(AuthError::MissingHeader)));
}

#[test]
fn basic_scheme_is_rejected() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = write_token_file(dir.path(), "current-tok\n");

  let result = verify_bearer(Some("Basic current-tok"), &path);
  assert!(matches!(result, Err(AuthError::InvalidScheme)));
}

#[test]
fn empty_bearer_value_is_rejected() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = write_token_file(dir.path(), "current-tok\n");

  let result = verify_bearer(Some("Bearer "), &path);
  assert!(matches!(result, Err(AuthError::EmptyToken)));
}

#[test]
fn unknown_token_is_rejected() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = write_token_file(dir.path(), "current-tok\n");

  let result = verify_bearer(Some("Bearer wrong-tok"), &path);
  assert!(matches!(result, Err(AuthError::TokenMismatch)));
}

#[test]
fn missing_token_file_is_rejected() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("does-not-exist");

  let result = verify_bearer(Some("Bearer whatever"), &path);
  assert!(matches!(result, Err(AuthError::TokenFile(_))));
}
