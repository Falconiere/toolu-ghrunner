use std::path::PathBuf;

use shared::RunnerError;

/// Metadata about a stored artifact.
#[derive(Debug, Clone)]
pub struct ArtifactEntry {
  pub id: u64,
  pub name: String,
  pub size: u64,
}

/// Backend storage abstraction for artifacts.
///
/// Implementations handle the actual storage (filesystem, S3, R2, etc.).
pub trait ArtifactBackend: Send + Sync {
  /// Create an artifact container (returns a container ID).
  fn create_container(
    &self,
    run_id: &str,
    name: &str,
  ) -> impl std::future::Future<Output = Result<String, RunnerError>> + Send;

  /// Upload a chunk of artifact data.
  fn upload_chunk(
    &self,
    run_id: &str,
    name: &str,
    chunk_index: u32,
    data: Vec<u8>,
  ) -> impl std::future::Future<Output = Result<(), RunnerError>> + Send;

  /// Finalize the artifact -- concatenate all chunks.
  fn finalize(
    &self,
    run_id: &str,
    name: &str,
  ) -> impl std::future::Future<Output = Result<(), RunnerError>> + Send;

  /// Download the full artifact content.
  fn download(
    &self,
    run_id: &str,
    name: &str,
  ) -> impl std::future::Future<Output = Result<Vec<u8>, RunnerError>> + Send;

  /// List all finalized artifacts for a run.
  fn list(
    &self,
    run_id: &str,
  ) -> impl std::future::Future<Output = Result<Vec<ArtifactEntry>, RunnerError>> + Send;
}

/// Filesystem-backed artifact storage.
///
/// Stores artifacts at `{root}/{run_id}/{artifact_name}/`.
pub struct LocalBackend {
  root: PathBuf,
  next_id: std::sync::atomic::AtomicU64,
}

impl LocalBackend {
  pub fn new(root: PathBuf) -> Self {
    Self {
      root,
      next_id: std::sync::atomic::AtomicU64::new(1),
    }
  }

  /// Validate a user-supplied artifact component (`run_id` or artifact
  /// `name`) before joining it under `self.root`. Rejects path separators
  /// (`/`, `\`), parent-dir traversal (`..`), NUL bytes, and absolute
  /// paths. Returns `RunnerError::Artifact` on the first violation.
  ///
  /// Every `LocalBackend` operation that touches a user-supplied string
  /// funnels through `artifact_dir` (and its callers), so a single
  /// validation here closes the path-traversal hole across all entry
  /// points (`create_container`, `upload_chunk`, `finalize`,
  /// `download`, `list`).
  fn validate_artifact_component(s: &str) -> Result<(), RunnerError> {
    if s.is_empty() {
      return Err(RunnerError::Artifact(
        "artifact component is empty".to_owned(),
      ));
    }
    if s.contains("..") {
      return Err(RunnerError::Artifact(format!(
        "artifact component contains parent-dir traversal: {s:?}"
      )));
    }
    if s.contains('/') || s.contains('\\') {
      return Err(RunnerError::Artifact(format!(
        "artifact component contains path separator: {s:?}"
      )));
    }
    if s.contains('\0') {
      return Err(RunnerError::Artifact(
        "artifact component contains NUL byte".to_owned(),
      ));
    }
    if std::path::Path::new(s).is_absolute() {
      return Err(RunnerError::Artifact(format!(
        "artifact component is an absolute path: {s:?}"
      )));
    }
    Ok(())
  }

  fn artifact_dir(&self, run_id: &str, name: &str) -> PathBuf {
    self.root.join(run_id).join(name)
  }

  fn chunk_path(&self, run_id: &str, name: &str, chunk_index: u32) -> PathBuf {
    self
      .artifact_dir(run_id, name)
      .join(format!("{chunk_index}.part"))
  }

  fn finalized_path(&self, run_id: &str, name: &str) -> PathBuf {
    self.artifact_dir(run_id, name).join("artifact.bin")
  }
}

impl ArtifactBackend for LocalBackend {
  async fn create_container(&self, run_id: &str, name: &str) -> Result<String, RunnerError> {
    Self::validate_artifact_component(run_id)?;
    Self::validate_artifact_component(name)?;
    let dir = self.artifact_dir(run_id, name);
    tokio::fs::create_dir_all(&dir)
      .await
      .map_err(RunnerError::Io)?;

    let id = self
      .next_id
      .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Ok(id.to_string())
  }

  async fn upload_chunk(
    &self,
    run_id: &str,
    name: &str,
    chunk_index: u32,
    data: Vec<u8>,
  ) -> Result<(), RunnerError> {
    Self::validate_artifact_component(run_id)?;
    Self::validate_artifact_component(name)?;
    let dir = self.artifact_dir(run_id, name);
    tokio::fs::create_dir_all(&dir)
      .await
      .map_err(RunnerError::Io)?;

    let path = self.chunk_path(run_id, name, chunk_index);
    tokio::fs::write(&path, &data)
      .await
      .map_err(RunnerError::Io)?;
    Ok(())
  }

  async fn finalize(&self, run_id: &str, name: &str) -> Result<(), RunnerError> {
    Self::validate_artifact_component(run_id)?;
    Self::validate_artifact_component(name)?;
    let dir = self.artifact_dir(run_id, name);
    let mut parts = collect_chunk_parts(&dir).await?;
    parts.sort_by_key(|(idx, _)| *idx);

    let mut combined = Vec::new();
    for (_, path) in &parts {
      let chunk = tokio::fs::read(path).await.map_err(RunnerError::Io)?;
      combined.extend_from_slice(&chunk);
    }

    let finalized = self.finalized_path(run_id, name);
    tokio::fs::write(&finalized, &combined)
      .await
      .map_err(RunnerError::Io)?;

    for (_, path) in &parts {
      tokio::fs::remove_file(path).await.ok();
    }

    Ok(())
  }

  async fn download(&self, run_id: &str, name: &str) -> Result<Vec<u8>, RunnerError> {
    Self::validate_artifact_component(run_id)?;
    Self::validate_artifact_component(name)?;
    let path = self.finalized_path(run_id, name);
    tokio::fs::read(&path).await.map_err(|e| {
      RunnerError::Artifact(format!(
        "artifact '{name}' not found for run '{run_id}': {e}"
      ))
    })
  }

  async fn list(&self, run_id: &str) -> Result<Vec<ArtifactEntry>, RunnerError> {
    Self::validate_artifact_component(run_id)?;
    let run_dir = self.root.join(run_id);
    if !run_dir.exists() {
      return Ok(Vec::new());
    }

    let mut entries = tokio::fs::read_dir(&run_dir)
      .await
      .map_err(RunnerError::Io)?;

    let mut artifacts = Vec::new();
    let mut id_counter = 1u64;

    loop {
      let entry = entries.next_entry().await.map_err(RunnerError::Io)?;
      let Some(entry) = entry else { break };

      if !entry.file_type().await.map_err(RunnerError::Io)?.is_dir() {
        continue;
      }

      let artifact_name = entry.file_name().to_string_lossy().to_string();
      let finalized = entry.path().join("artifact.bin");

      if finalized.exists() {
        let metadata = tokio::fs::metadata(&finalized)
          .await
          .map_err(RunnerError::Io)?;

        artifacts.push(ArtifactEntry {
          id: id_counter,
          name: artifact_name,
          size: metadata.len(),
        });
        id_counter += 1;
      }
    }

    Ok(artifacts)
  }
}

async fn collect_chunk_parts(dir: &std::path::Path) -> Result<Vec<(u32, PathBuf)>, RunnerError> {
  let mut entries = tokio::fs::read_dir(dir).await.map_err(RunnerError::Io)?;
  let mut parts = Vec::new();

  loop {
    let entry = entries.next_entry().await.map_err(RunnerError::Io)?;
    let Some(entry) = entry else { break };
    let file_name = entry.file_name();
    let name_str = file_name.to_string_lossy();
    if let Some(idx_str) = name_str.strip_suffix(".part")
      && let Ok(idx) = idx_str.parse::<u32>()
    {
      parts.push((idx, entry.path()));
    }
  }

  Ok(parts)
}

#[cfg(test)]
mod tests {
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
}
