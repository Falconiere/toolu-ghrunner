//! Cache garbage collection: TTL expiry, `max_bytes` eviction, and an
//! unreferenced-chunk sweep guarded by live-restore read leases.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Duration, Utc};
use shared::RunnerError;

use super::index::{CacheIndex, IndexEntry, IndexRecord};
use super::manifest::ChunkId;
use super::store::CasStore;

/// Refcounted set of chunk ids a live restore is reading; GC never deletes a leased chunk.
#[derive(Clone, Default)]
pub struct LeaseSet {
  counts: Arc<Mutex<HashMap<ChunkId, usize>>>,
}

impl LeaseSet {
  /// Create an empty lease set.
  pub fn new() -> Self {
    Self::default()
  }

  /// Lease `ids` for the returned guard's lifetime, incrementing each refcount.
  pub fn acquire(&self, ids: &[ChunkId]) -> LeaseGuard {
    if let Ok(mut map) = self.counts.lock() {
      for id in ids {
        *map.entry(id.clone()).or_insert(0) += 1;
      }
    }
    LeaseGuard {
      set: self.clone(),
      ids: ids.to_vec(),
    }
  }

  /// True if `id` is currently leased by at least one live restore.
  pub fn is_leased(&self, id: &ChunkId) -> bool {
    self
      .counts
      .lock()
      .map(|map| map.get(id).is_some_and(|&n| n > 0))
      .unwrap_or(false)
  }
}

/// Drops the leases taken by `LeaseSet::acquire`, decrementing each refcount.
pub struct LeaseGuard {
  set: LeaseSet,
  ids: Vec<ChunkId>,
}

impl Drop for LeaseGuard {
  fn drop(&mut self) {
    if let Ok(mut map) = self.set.counts.lock() {
      for id in &self.ids {
        if let Some(n) = map.get_mut(id) {
          *n = n.saturating_sub(1);
          if *n == 0 {
            map.remove(id);
          }
        }
      }
    }
  }
}

/// What one GC pass removed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GcReport {
  /// Index entries dropped for exceeding the TTL.
  pub entries_expired: usize,
  /// Index entries dropped to bring live size under `max_bytes`.
  pub entries_evicted: usize,
  /// Chunk blobs swept because they were unreferenced and unleased.
  pub chunks_deleted: usize,
  /// Manifest blobs swept because no retained entry referenced them.
  pub manifests_deleted: usize,
}

/// Garbage collector parameterized by a TTL and a live-size cap.
pub struct CacheGc {
  ttl_days: u64,
  max_bytes: u64,
}

/// The retained entries per `(scope, version)` after TTL + cap pruning.
struct Retention {
  groups: HashMap<(String, String), Vec<IndexEntry>>,
  expired: usize,
  evicted: usize,
}

impl CacheGc {
  /// Create a collector expiring entries older than `ttl_days` and capping live size at `max_bytes`.
  pub fn new(ttl_days: u64, max_bytes: u64) -> Self {
    Self {
      ttl_days,
      max_bytes,
    }
  }

  /// Run one GC pass: rewrite the index, then sweep unreferenced blobs.
  ///
  /// The index is rewritten first so the swept live set reflects the
  /// post-eviction state. A leased chunk is never deleted.
  ///
  /// # Errors
  /// `RunnerError::Io`/`Json`/`Cache` if the index or a blob cannot be read,
  /// rewritten, or removed.
  pub async fn run(
    &self,
    store: &CasStore,
    index: &CacheIndex,
    leases: &LeaseSet,
  ) -> Result<GcReport, RunnerError> {
    let ttl = i64::try_from(self.ttl_days)
      .map_err(|e| RunnerError::Cache(format!("ttl_days overflow: {e}")))?;
    let cutoff = Utc::now() - Duration::days(ttl);
    let records = index.records()?;
    let retention = plan_retention(&records, cutoff, self.max_bytes);
    rewrite_groups(index, &records, &retention)?;
    let live_manifests: HashSet<ChunkId> = retention
      .groups
      .values()
      .flatten()
      .map(|entry| entry.manifest.clone())
      .collect();
    let (chunks_deleted, manifests_deleted) = sweep(store, &live_manifests, leases).await?;
    Ok(GcReport {
      entries_expired: retention.expired,
      entries_evicted: retention.evicted,
      chunks_deleted,
      manifests_deleted,
    })
  }
}

/// Partition records into the retained set, counting TTL expiries and cap evictions.
fn plan_retention(records: &[IndexRecord], cutoff: DateTime<Utc>, max_bytes: u64) -> Retention {
  let mut expired = 0usize;
  let mut alive: Vec<&IndexRecord> = Vec::new();
  for rec in records {
    if rec.entry.created_at < cutoff {
      expired += 1;
    } else {
      alive.push(rec);
    }
  }
  let evicted = evict_over_cap(&mut alive, max_bytes);
  let mut groups: HashMap<(String, String), Vec<IndexEntry>> = HashMap::new();
  for rec in alive {
    groups
      .entry((rec.scope.clone(), rec.version.clone()))
      .or_default()
      .push(rec.entry.clone());
  }
  Retention {
    groups,
    expired,
    evicted,
  }
}

/// Drop the oldest-created records until the retained total fits `max_bytes`; returns the count dropped.
fn evict_over_cap(alive: &mut Vec<&IndexRecord>, max_bytes: u64) -> usize {
  let mut running: u64 = alive
    .iter()
    .map(|rec| rec.entry.size_bytes)
    .fold(0u64, u64::saturating_add);
  if running <= max_bytes {
    return 0;
  }
  alive.sort_by_key(|rec| rec.entry.created_at);
  let mut cut = 0usize;
  while running > max_bytes {
    match alive.get(cut) {
      Some(rec) => {
        running = running.saturating_sub(rec.entry.size_bytes);
        cut += 1;
      },
      None => break,
    }
  }
  alive.drain(0..cut);
  cut
}

/// Rewrite every `(scope, version)` seen in `records` to its retained entries.
fn rewrite_groups(
  index: &CacheIndex,
  records: &[IndexRecord],
  retention: &Retention,
) -> Result<(), RunnerError> {
  let mut keys: HashSet<(&str, &str)> = HashSet::new();
  for rec in records {
    keys.insert((rec.scope.as_str(), rec.version.as_str()));
  }
  for (scope, version) in keys {
    let entries = retention
      .groups
      .get(&(scope.to_owned(), version.to_owned()))
      .map(Vec::as_slice)
      .unwrap_or(&[]);
    index.rewrite(scope, version, entries)?;
  }
  Ok(())
}

/// Delete every manifest not in `live_manifests` and every chunk they don't reference and no lease holds.
async fn sweep(
  store: &CasStore,
  live_manifests: &HashSet<ChunkId>,
  leases: &LeaseSet,
) -> Result<(usize, usize), RunnerError> {
  let mut live_chunks: HashSet<ChunkId> = HashSet::new();
  for mid in live_manifests {
    for chunk in store.get_manifest(mid).await?.chunks {
      live_chunks.insert(chunk.id);
    }
  }
  let mut manifests_deleted = 0usize;
  for mid in store.list_manifest_ids()? {
    if !live_manifests.contains(&mid) {
      store.delete_manifest(&mid).await?;
      manifests_deleted += 1;
    }
  }
  let mut chunks_deleted = 0usize;
  for cid in store.list_chunk_ids()? {
    if !live_chunks.contains(&cid) && !leases.is_leased(&cid) {
      store.delete_chunk(&cid).await?;
      chunks_deleted += 1;
    }
  }
  Ok((chunks_deleted, manifests_deleted))
}
