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
    let path = self.finalized_path(run_id, name);
    tokio::fs::read(&path).await.map_err(|e| {
      RunnerError::Artifact(format!(
        "artifact '{name}' not found for run '{run_id}': {e}"
      ))
    })
  }

  async fn list(&self, run_id: &str) -> Result<Vec<ArtifactEntry>, RunnerError> {
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
