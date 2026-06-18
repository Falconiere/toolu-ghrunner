//! S3-compatible remote cache backend using OpenDAL.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use opendal::{Operator, services};
use shared::RunnerError;
use tokio::sync::RwLock;

use super::traits::{CacheBackend, CacheEntry};

struct PendingEntry {
  key: String,
  version: String,
  data: Vec<u8>,
}

/// Remote cache backend supporting any S3-compatible endpoint via OpenDAL.
pub struct RemoteBackend {
  op: Operator,
  next_id: AtomicU64,
  pending: RwLock<HashMap<u64, PendingEntry>>,
}

/// Configuration for the remote backend.
pub struct RemoteConfig {
  pub bucket: String,
  pub endpoint: String,
  pub region: String,
  pub access_key_id: String,
  pub secret_access_key: String,
}

impl RemoteBackend {
  /// Create a new S3-compatible remote backend.
  ///
  /// # Errors
  ///
  /// Returns `RunnerError::Cache` if the operator cannot be built.
  pub fn new(config: &RemoteConfig) -> Result<Self, RunnerError> {
    let builder = services::S3::default()
      .bucket(&config.bucket)
      .endpoint(&config.endpoint)
      .region(&config.region)
      .access_key_id(&config.access_key_id)
      .secret_access_key(&config.secret_access_key);

    let op = Operator::new(builder)
      .map_err(|e| RunnerError::Cache(format!("failed to build S3 operator: {e}")))?
      .finish();

    Ok(Self {
      op,
      next_id: AtomicU64::new(1),
      pending: RwLock::new(HashMap::new()),
    })
  }

  fn object_path(key: &str, version: &str) -> String {
    format!("cache/{key}/{version}")
  }
}

impl CacheBackend for RemoteBackend {
  async fn lookup(&self, key: &str, version: &str) -> Result<Option<CacheEntry>, RunnerError> {
    let path = Self::object_path(key, version);
    match self.op.stat(&path).await {
      Ok(meta) => Ok(Some(CacheEntry {
        id: 0,
        key: key.to_owned(),
        version: version.to_owned(),
        size: meta.content_length(),
      })),
      Err(e) if e.kind() == opendal::ErrorKind::NotFound => Ok(None),
      Err(e) => Err(RunnerError::Cache(format!("remote lookup failed: {e}"))),
    }
  }

  async fn reserve(&self, key: &str, version: &str) -> Result<u64, RunnerError> {
    let id = self.next_id.fetch_add(1, Ordering::Relaxed);
    let entry = PendingEntry {
      key: key.to_owned(),
      version: version.to_owned(),
      data: Vec::new(),
    };
    self.pending.write().await.insert(id, entry);
    Ok(id)
  }

  async fn upload_chunk(
    &self,
    cache_id: u64,
    _offset: u64,
    data: Vec<u8>,
  ) -> Result<(), RunnerError> {
    let mut pending = self.pending.write().await;
    let entry = pending
      .get_mut(&cache_id)
      .ok_or_else(|| RunnerError::Cache(format!("no pending entry for id {cache_id}")))?;
    entry.data.extend_from_slice(&data);
    Ok(())
  }

  async fn finalize(&self, cache_id: u64, _size: u64) -> Result<(), RunnerError> {
    let entry = self
      .pending
      .write()
      .await
      .remove(&cache_id)
      .ok_or_else(|| RunnerError::Cache(format!("no pending entry for id {cache_id}")))?;

    let path = Self::object_path(&entry.key, &entry.version);
    self
      .op
      .write(&path, entry.data)
      .await
      .map_err(|e| RunnerError::Cache(format!("remote write failed: {e}")))?;
    Ok(())
  }

  async fn download(&self, _cache_id: u64) -> Result<Vec<u8>, RunnerError> {
    // For remote backend, download by ID requires mapping ID→path.
    // Since IDs are ephemeral (in-memory), callers should use lookup + download by key.
    Err(RunnerError::Cache(
      "remote download by ID not supported — use lookup first".into(),
    ))
  }

  async fn list(&self) -> Result<Vec<CacheEntry>, RunnerError> {
    let mut entries = Vec::new();
    let lister = self
      .op
      .list("cache/")
      .await
      .map_err(|e| RunnerError::Cache(format!("remote list failed: {e}")))?;

    for entry in lister {
      let meta = entry.metadata().content_length();
      entries.push(CacheEntry {
        id: 0,
        key: entry.name().to_owned(),
        version: String::new(),
        size: meta,
      });
    }
    Ok(entries)
  }
}
