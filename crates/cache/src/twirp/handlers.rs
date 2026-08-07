//! The three Twirp `CacheService` RPC handlers, one file per method.

/// `CreateCacheEntry`: mint an upload URL, or refuse.
pub mod create;
/// `GetCacheEntryDownloadURL`: a signed download URL, or a bare miss.
pub mod download;
/// `FinalizeCacheEntryUpload`: ingest + index, or a bare failure.
pub mod finalize;
