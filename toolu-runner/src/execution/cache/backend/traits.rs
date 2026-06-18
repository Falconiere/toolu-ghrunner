//! Cache backend trait and entry metadata types.

use shared::RunnerError;

/// Metadata about a cached entry.
#[derive(Debug, Clone)]
pub struct CacheEntry {
  pub id: u64,
  pub key: String,
  pub version: String,
  pub size: u64,
}

/// Backend storage abstraction for the cache service.
pub trait CacheBackend: Send + Sync {
  /// Look up a cache entry by key and version (exact then prefix match).
  fn lookup(
    &self,
    key: &str,
    version: &str,
  ) -> impl std::future::Future<Output = Result<Option<CacheEntry>, RunnerError>> + Send;

  /// Reserve a new cache entry. Returns a cache ID for uploading chunks.
  fn reserve(
    &self,
    key: &str,
    version: &str,
  ) -> impl std::future::Future<Output = Result<u64, RunnerError>> + Send;

  /// Upload a chunk of cache data at the given byte offset.
  fn upload_chunk(
    &self,
    cache_id: u64,
    offset: u64,
    data: Vec<u8>,
  ) -> impl std::future::Future<Output = Result<(), RunnerError>> + Send;

  /// Finalize the cache entry with the total size.
  fn finalize(
    &self,
    cache_id: u64,
    size: u64,
  ) -> impl std::future::Future<Output = Result<(), RunnerError>> + Send;

  /// Download the full cache content by ID.
  fn download(
    &self,
    cache_id: u64,
  ) -> impl std::future::Future<Output = Result<Vec<u8>, RunnerError>> + Send;

  /// List all finalized cache entries.
  fn list(&self) -> impl std::future::Future<Output = Result<Vec<CacheEntry>, RunnerError>> + Send;
}
