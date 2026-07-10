//! Content-addressed store (CAS): FastCDC chunks keyed by BLAKE3.

mod chunk_io;
mod chunker;
/// Garbage collection: TTL, `max_bytes` eviction, and unreferenced-chunk sweep.
pub mod gc;
/// Persistent, restart-safe cache index over `(scope, version)` JSONL logs.
pub mod index;
/// Chunk ids, chunk refs, and the ranged-read manifest.
pub mod manifest;
/// The `CasStore`: ingest, ranged read, manifest persistence.
pub mod store;

pub use gc::{CacheGc, GcReport, LeaseGuard, LeaseSet};
pub use index::{CacheIndex, IndexEntry, IndexRecord, entry_id_for};
pub use manifest::{ChunkId, ChunkRef, Manifest};
pub use store::CasStore;
