//! Filesystem-backed cache with LRU eviction.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use shared::RunnerError;
use tokio::sync::RwLock;

use super::traits::{CacheBackend, CacheEntry};

struct ReservedEntry {
  key: String,
  version: String,
  finalized: bool,
  size: u64,
  last_access: Instant,
}

impl ReservedEntry {
  fn to_cache_entry(&self, id: u64) -> CacheEntry {
    CacheEntry {
      id,
      key: self.key.clone(),
      version: self.version.clone(),
      size: self.size,
    }
  }
}

/// Filesystem-backed cache with LRU eviction.
pub struct LocalDiskBackend {
  root: PathBuf,
  max_size: u64,
  next_id: AtomicU64,
  entries: RwLock<HashMap<u64, ReservedEntry>>,
}

impl LocalDiskBackend {
  pub fn new(root: PathBuf, max_size: u64) -> Self {
    Self {
      root,
      max_size,
      next_id: AtomicU64::new(1),
      entries: RwLock::new(HashMap::new()),
    }
  }

  fn entry_dir(&self, cache_id: u64) -> PathBuf {
    self.root.join(cache_id.to_string())
  }

  fn data_path(&self, cache_id: u64) -> PathBuf {
    self.entry_dir(cache_id).join("data")
  }

  async fn evict_if_needed(&self) -> Result<(), RunnerError> {
    let mut entries = self.entries.write().await;

    let total_size: u64 = entries
      .values()
      .filter(|e| e.finalized)
      .map(|e| e.size)
      .sum();

    if total_size <= self.max_size {
      return Ok(());
    }

    let mut finalized: Vec<(u64, Instant, u64)> = entries
      .iter()
      .filter(|(_, e)| e.finalized)
      .map(|(id, e)| (*id, e.last_access, e.size))
      .collect();
    finalized.sort_by_key(|(_, access, _)| *access);

    let mut current_size = total_size;
    for (id, _, size) in &finalized {
      if current_size <= self.max_size {
        break;
      }
      let dir = self.entry_dir(*id);
      tokio::fs::remove_dir_all(&dir).await.ok();
      entries.remove(id);
      current_size = current_size.saturating_sub(*size);
    }

    Ok(())
  }
}

impl CacheBackend for LocalDiskBackend {
  async fn lookup(&self, key: &str, version: &str) -> Result<Option<CacheEntry>, RunnerError> {
    let mut entries = self.entries.write().await;

    // Exact match first
    for (id, entry) in entries.iter_mut() {
      if entry.finalized && entry.key == key && entry.version == version {
        entry.last_access = Instant::now();
        return Ok(Some(entry.to_cache_entry(*id)));
      }
    }

    // Prefix match -- find the most recently accessed
    let best_id = entries
      .iter()
      .filter(|(_, e)| e.finalized && e.version == version && e.key.starts_with(key))
      .max_by_key(|(_, e)| e.last_access)
      .map(|(id, _)| *id);

    if let Some(id) = best_id
      && let Some(entry) = entries.get_mut(&id)
    {
      entry.last_access = Instant::now();
      return Ok(Some(entry.to_cache_entry(id)));
    }

    Ok(None)
  }

  async fn reserve(&self, key: &str, version: &str) -> Result<u64, RunnerError> {
    let id = self.next_id.fetch_add(1, Ordering::Relaxed);
    let dir = self.entry_dir(id);
    tokio::fs::create_dir_all(&dir)
      .await
      .map_err(RunnerError::Io)?;

    let mut entries = self.entries.write().await;
    entries.insert(
      id,
      ReservedEntry {
        key: key.to_owned(),
        version: version.to_owned(),
        finalized: false,
        size: 0,
        last_access: Instant::now(),
      },
    );

    Ok(id)
  }

  async fn upload_chunk(
    &self,
    cache_id: u64,
    offset: u64,
    data: Vec<u8>,
  ) -> Result<(), RunnerError> {
    use tokio::io::{AsyncSeekExt, AsyncWriteExt};

    let path = self.data_path(cache_id);
    let mut file = tokio::fs::OpenOptions::new()
      .create(true)
      .truncate(false)
      .write(true)
      .open(&path)
      .await
      .map_err(RunnerError::Io)?;

    file
      .seek(std::io::SeekFrom::Start(offset))
      .await
      .map_err(RunnerError::Io)?;
    file.write_all(&data).await.map_err(RunnerError::Io)?;
    Ok(())
  }

  async fn finalize(&self, cache_id: u64, size: u64) -> Result<(), RunnerError> {
    {
      let mut entries = self.entries.write().await;
      let entry = entries
        .get_mut(&cache_id)
        .ok_or_else(|| RunnerError::Cache(format!("cache entry {cache_id} not found")))?;
      entry.finalized = true;
      entry.size = size;
      entry.last_access = Instant::now();
    }
    self.evict_if_needed().await
  }

  async fn download(&self, cache_id: u64) -> Result<Vec<u8>, RunnerError> {
    let entries = self.entries.read().await;
    let entry = entries
      .get(&cache_id)
      .ok_or_else(|| RunnerError::Cache(format!("cache entry {cache_id} not found")))?;
    if !entry.finalized {
      return Err(RunnerError::Cache(format!(
        "cache entry {cache_id} not yet finalized"
      )));
    }
    drop(entries);

    let path = self.data_path(cache_id);
    tokio::fs::read(&path)
      .await
      .map_err(|e| RunnerError::Cache(format!("failed to read cache {cache_id}: {e}")))
  }

  async fn list(&self) -> Result<Vec<CacheEntry>, RunnerError> {
    let entries = self.entries.read().await;
    Ok(
      entries
        .iter()
        .filter(|(_, e)| e.finalized)
        .map(|(id, e)| e.to_cache_entry(*id))
        .collect(),
    )
  }
}
