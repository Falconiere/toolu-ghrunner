//! Tests for `backend`: path-traversal / validation guards on `LocalBackend`.

use super::*;
use std::env;

fn tmp_root() -> PathBuf {
  let mut p = env::temp_dir();
  p.push(format!("toolu-artifact-test-{}", uuid::Uuid::new_v4()));
  p
}

#[tokio::test]
async fn create_container_rejects_parent_dir_in_name() {
  let root = tmp_root();
  let backend = LocalBackend::new(root.clone());
  let result = backend.create_container("run-1", "../etc/pwn").await;
  assert!(matches!(result, Err(RunnerError::Artifact(_))));
  // Defensive: root must not have been created at all.
  assert!(!root.join("run-1").exists());
}

#[tokio::test]
async fn create_container_rejects_path_separator_in_name() {
  let backend = LocalBackend::new(tmp_root());
  let result = backend.create_container("run-1", "sub/dir").await;
  assert!(matches!(result, Err(RunnerError::Artifact(_))));
}

#[tokio::test]
async fn create_container_rejects_path_separator_in_run_id() {
  let backend = LocalBackend::new(tmp_root());
  let result = backend
    .create_container("run/../../escape", "artifact")
    .await;
  assert!(matches!(result, Err(RunnerError::Artifact(_))));
}

#[tokio::test]
async fn create_container_rejects_empty_name() {
  let backend = LocalBackend::new(tmp_root());
  let result = backend.create_container("run-1", "").await;
  assert!(matches!(result, Err(RunnerError::Artifact(_))));
}

#[tokio::test]
async fn create_container_rejects_absolute_path() {
  let backend = LocalBackend::new(tmp_root());
  let result = backend.create_container("run-1", "/etc/passwd").await;
  assert!(matches!(result, Err(RunnerError::Artifact(_))));
}

#[tokio::test]
async fn create_container_rejects_nul_byte() {
  let backend = LocalBackend::new(tmp_root());
  let result = backend.create_container("run-1", "foo\0bar").await;
  assert!(matches!(result, Err(RunnerError::Artifact(_))));
}

#[tokio::test]
async fn create_container_accepts_valid_name() {
  let root = tmp_root();
  let backend = LocalBackend::new(root.clone());
  let result = backend.create_container("run-1", "build-output").await;
  assert!(result.is_ok(), "valid name was rejected: {result:?}");
  // The directory must be the one expected under the root, not a
  // traversal of the root.
  assert!(root.join("run-1").join("build-output").exists());
}

#[tokio::test]
async fn upload_chunk_rejects_traversal() {
  let root = tmp_root();
  let backend = LocalBackend::new(root);
  let result = backend
    .upload_chunk("run-1", "..", 0, b"data".to_vec())
    .await;
  assert!(matches!(result, Err(RunnerError::Artifact(_))));
}

#[tokio::test]
async fn finalize_rejects_traversal() {
  let backend = LocalBackend::new(tmp_root());
  let result = backend.finalize("run-1", "../etc/pwn").await;
  assert!(matches!(result, Err(RunnerError::Artifact(_))));
}

#[tokio::test]
async fn download_rejects_traversal() {
  let backend = LocalBackend::new(tmp_root());
  let result = backend.download("run-1", "../etc/pwn").await;
  assert!(matches!(result, Err(RunnerError::Artifact(_))));
}

#[tokio::test]
async fn list_rejects_traversal() {
  let backend = LocalBackend::new(tmp_root());
  let result = backend.list("../etc").await;
  assert!(matches!(result, Err(RunnerError::Artifact(_))));
}
