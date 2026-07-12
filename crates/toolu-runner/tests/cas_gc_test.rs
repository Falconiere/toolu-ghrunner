//! Real-data cache GC tests: TTL expiry, shared-chunk retention, read leases,
//! `max_bytes` eviction, and `GcReport` accounting. Payloads are real tars of
//! this repo's `shared/src` and `protocol/src`; no mocks.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use cache::cas::{
  CacheGc, CacheIndex, CasStore, ChunkId, GcReport, IndexEntry, LeaseSet, Manifest,
};
use chrono::{DateTime, Duration, Utc};

/// Boxed error alias for test helpers that use `?`.
type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

/// Absolute path to `rel` under the workspace root (parent of this crate).
fn repo(rel: &str) -> PathBuf {
  Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(rel)
}

/// Tar `src` into an in-memory buffer of real bytes.
fn tar_dir(src: &Path) -> TestResult<Vec<u8>> {
  let mut builder = tar::Builder::new(Vec::new());
  builder.append_dir_all("root", src)?;
  Ok(builder.into_inner()?)
}

/// Write `bytes` to `path`, ingest it, and persist its manifest; return `(manifest id, manifest)`.
async fn ingest_payload(
  store: &CasStore,
  bytes: &[u8],
  path: &Path,
) -> TestResult<(ChunkId, Manifest)> {
  std::fs::write(path, bytes)?;
  let manifest = store.ingest(path).await?;
  let mid = store.put_manifest(&manifest).await?;
  Ok((mid, manifest))
}

/// Build an `IndexEntry` from its parts.
fn entry(key: &str, mid: ChunkId, size: u64, created_at: DateTime<Utc>) -> IndexEntry {
  IndexEntry {
    key: key.to_owned(),
    manifest: mid,
    size_bytes: size,
    created_at,
  }
}

/// The set of chunk ids a manifest references.
fn chunk_ids(m: &Manifest) -> HashSet<ChunkId> {
  m.chunks.iter().map(|c| c.id.clone()).collect()
}

/// The single-scope read ladder for a scope name.
fn ladder(scope: &str) -> Vec<String> {
  vec![scope.to_owned()]
}

/// A fresh store (avg 2 KiB chunks) and index rooted in one tempdir.
fn setup() -> TestResult<(tempfile::TempDir, CasStore, CacheIndex)> {
  let dir = tempfile::tempdir()?;
  let store = CasStore::new(dir.path().join("cas"), 2048, 1 << 30);
  let index = CacheIndex::new(dir.path().join("idx"));
  Ok((dir, store, index))
}

#[tokio::test]
async fn ttl_expires_old_entry_and_sweeps_its_chunks() -> TestResult<()> {
  let (dir, store, index) = setup()?;
  let scope = "refs/heads/main";
  let version = "v1";
  let fresh_tar = tar_dir(&repo("shared/src"))?;
  let old_tar = tar_dir(&repo("protocol/src"))?;
  let (fresh_mid, fresh_m) =
    ingest_payload(&store, &fresh_tar, &dir.path().join("fresh.tar")).await?;
  let (old_mid, old_m) = ingest_payload(&store, &old_tar, &dir.path().join("old.tar")).await?;
  index.insert(
    scope,
    version,
    &entry("fresh", fresh_mid, fresh_m.total_size, Utc::now()),
  )?;
  index.insert(
    scope,
    version,
    &entry(
      "old",
      old_mid,
      old_m.total_size,
      Utc::now() - Duration::days(10),
    ),
  )?;

  let leases = LeaseSet::new();
  CacheGc::new(7, 1 << 30)
    .run(&store, &index, &leases)
    .await?;

  assert!(
    index.lookup(&ladder(scope), version, "old", &[])?.is_none(),
    "the 10-day-old entry must be expired at ttl=7"
  );
  assert!(
    index
      .lookup(&ladder(scope), version, "fresh", &[])?
      .is_some(),
    "the fresh entry must survive"
  );
  let fresh_set = chunk_ids(&fresh_m);
  let old_set = chunk_ids(&old_m);
  for id in old_set.difference(&fresh_set) {
    assert!(
      !store.has_chunk(id).await,
      "unreferenced old chunk should be swept"
    );
  }
  for id in &fresh_set {
    assert!(store.has_chunk(id).await, "fresh chunk must survive");
  }
  Ok(())
}

#[tokio::test]
async fn shared_chunks_survive_when_one_entry_expires() -> TestResult<()> {
  let (dir, store, index) = setup()?;
  let scope = "refs/heads/main";
  let version = "v1";
  // B = A ++ tail, so B shares A's leading chunks and adds exclusive tail chunks.
  let common = tar_dir(&repo("shared/src"))?;
  let tail = tar_dir(&repo("protocol/src"))?;
  let mut extended = Vec::new();
  extended.extend_from_slice(&common);
  extended.extend_from_slice(&tail);
  let (a_mid, a_m) = ingest_payload(&store, &common, &dir.path().join("a.tar")).await?;
  let (b_mid, b_m) = ingest_payload(&store, &extended, &dir.path().join("b.tar")).await?;

  // Keep A (fresh, the prefix); expire B (the extension).
  index.insert(
    scope,
    version,
    &entry("keep", a_mid, a_m.total_size, Utc::now()),
  )?;
  index.insert(
    scope,
    version,
    &entry(
      "drop",
      b_mid,
      b_m.total_size,
      Utc::now() - Duration::days(10),
    ),
  )?;

  let a_set = chunk_ids(&a_m);
  let b_set = chunk_ids(&b_m);
  let shared: Vec<ChunkId> = a_set.intersection(&b_set).cloned().collect();
  let exclusive_b: Vec<ChunkId> = b_set.difference(&a_set).cloned().collect();
  assert!(!shared.is_empty(), "the two manifests must share chunks");
  assert!(!exclusive_b.is_empty(), "B must have exclusive tail chunks");

  CacheGc::new(7, 1 << 30)
    .run(&store, &index, &LeaseSet::new())
    .await?;

  for id in &shared {
    assert!(
      store.has_chunk(id).await,
      "a shared chunk must survive via the live entry"
    );
  }
  for id in &exclusive_b {
    assert!(
      !store.has_chunk(id).await,
      "B's exclusive chunk must be swept"
    );
  }
  Ok(())
}

#[tokio::test]
async fn lease_protects_chunks_until_dropped() -> TestResult<()> {
  let (dir, store, index) = setup()?;
  let scope = "refs/heads/main";
  let version = "v1";
  let tar = tar_dir(&repo("shared/src"))?;
  let (mid, m) = ingest_payload(&store, &tar, &dir.path().join("x.tar")).await?;
  index.insert(
    scope,
    version,
    &entry("leased", mid, m.total_size, Utc::now() - Duration::days(10)),
  )?;
  let chunks: Vec<ChunkId> = m.chunks.iter().map(|c| c.id.clone()).collect();

  let leases = LeaseSet::new();
  let guard = leases.acquire(&chunks);
  CacheGc::new(7, 1 << 30)
    .run(&store, &index, &leases)
    .await?;
  for id in &chunks {
    assert!(
      store.has_chunk(id).await,
      "a leased chunk must never be swept"
    );
  }

  drop(guard);
  CacheGc::new(7, 1 << 30)
    .run(&store, &index, &leases)
    .await?;
  for id in &chunks {
    assert!(
      !store.has_chunk(id).await,
      "an unleased, unreferenced chunk must be swept"
    );
  }
  Ok(())
}

#[tokio::test]
async fn max_bytes_evicts_oldest_entries() -> TestResult<()> {
  let (dir, store, index) = setup()?;
  let scope = "refs/heads/main";
  let version = "v1";
  let tar = tar_dir(&repo("shared/src"))?;
  let (mid, m) = ingest_payload(&store, &tar, &dir.path().join("p.tar")).await?;
  let size = m.total_size;
  // Four entries of size S each; cap at 2S so the two newest survive.
  let now = Utc::now();
  for (i, key) in ["e0", "e1", "e2", "e3"].iter().enumerate() {
    let age = i64::try_from(4 - i)?;
    let created = now - Duration::seconds(age * 10);
    index.insert(scope, version, &entry(key, mid.clone(), size, created))?;
  }
  let max_bytes = size.saturating_mul(2);

  CacheGc::new(3650, max_bytes)
    .run(&store, &index, &LeaseSet::new())
    .await?;

  for gone in ["e0", "e1"] {
    assert!(
      index.lookup(&ladder(scope), version, gone, &[])?.is_none(),
      "the oldest entries must be evicted under the cap"
    );
  }
  for kept in ["e2", "e3"] {
    assert!(
      index.lookup(&ladder(scope), version, kept, &[])?.is_some(),
      "the newest entries must survive the cap"
    );
  }
  let retained: u64 = index
    .records()?
    .iter()
    .map(|r| r.entry.size_bytes)
    .fold(0u64, u64::saturating_add);
  assert!(
    retained <= max_bytes,
    "retained size must be within max_bytes"
  );
  Ok(())
}

#[tokio::test]
async fn report_counts_match_the_ttl_sweep() -> TestResult<()> {
  let (dir, store, index) = setup()?;
  let scope = "refs/heads/main";
  let version = "v1";
  let fresh_tar = tar_dir(&repo("shared/src"))?;
  let old_tar = tar_dir(&repo("protocol/src"))?;
  let (fresh_mid, fresh_m) = ingest_payload(&store, &fresh_tar, &dir.path().join("f.tar")).await?;
  let (old_mid, old_m) = ingest_payload(&store, &old_tar, &dir.path().join("o.tar")).await?;
  assert_ne!(
    fresh_mid, old_mid,
    "distinct payloads must yield distinct manifests"
  );
  index.insert(
    scope,
    version,
    &entry("fresh", fresh_mid, fresh_m.total_size, Utc::now()),
  )?;
  index.insert(
    scope,
    version,
    &entry(
      "old",
      old_mid,
      old_m.total_size,
      Utc::now() - Duration::days(10),
    ),
  )?;

  let expected_chunks = chunk_ids(&old_m).difference(&chunk_ids(&fresh_m)).count();
  let report: GcReport = CacheGc::new(7, 1 << 30)
    .run(&store, &index, &LeaseSet::new())
    .await?;

  assert_eq!(report.entries_expired, 1, "one entry exceeded the TTL");
  assert_eq!(report.entries_evicted, 0, "the cap was not exceeded");
  assert_eq!(
    report.manifests_deleted, 1,
    "the expired entry's manifest is orphaned"
  );
  assert_eq!(
    report.chunks_deleted, expected_chunks,
    "exactly the exclusively-unreferenced chunks are swept"
  );
  Ok(())
}
