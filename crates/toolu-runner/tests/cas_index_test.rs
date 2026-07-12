//! Real-data tests for the persistent cache index: ladder resolution,
//! exact/restore precedence, version isolation, torn-tail tolerance,
//! restart-safety, and the JS-safe `entry_id`.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::DateTime;
use cache::cas::{CacheIndex, ChunkId, IndexEntry, entry_id_for};

/// Boxed error alias for test helpers that use `?`.
type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

/// A real BLAKE3 chunk id derived from `seed`.
fn chunk_id(seed: &[u8]) -> ChunkId {
  ChunkId(*blake3::hash(seed).as_bytes())
}

/// Build an `IndexEntry` with a real digest and a fixed `created_at` (epoch secs).
fn mk_entry(key: &str, secs: i64) -> TestResult<IndexEntry> {
  let created_at = DateTime::from_timestamp(secs, 0).ok_or("timestamp out of range")?;
  Ok(IndexEntry {
    key: key.to_owned(),
    manifest: chunk_id(key.as_bytes()),
    size_bytes: 1024,
    created_at,
  })
}

/// The single write-scope ladder for a scope name.
fn ladder(scope: &str) -> Vec<String> {
  vec![scope.to_owned()]
}

/// First `.jsonl` file found under `dir`, recursing into subdirs.
fn find_jsonl(dir: &Path) -> TestResult<Option<PathBuf>> {
  for entry in fs::read_dir(dir)? {
    let path = entry?.path();
    if path.is_dir() {
      if let Some(found) = find_jsonl(&path)? {
        return Ok(Some(found));
      }
    } else if path.extension().is_some_and(|ext| ext == "jsonl") {
      return Ok(Some(path));
    }
  }
  Ok(None)
}

#[test]
fn ladder_resolves_via_secondary_scope() -> TestResult<()> {
  let dir = tempfile::tempdir()?;
  let idx = CacheIndex::new(dir.path().to_path_buf());
  let version = "sha256-abc";
  idx.insert("refs/heads/main", version, &mk_entry("build-cache", 1_000)?)?;
  // The feature scope holds nothing; the ladder must fall through to main.
  let read_ladder = vec![
    "refs/heads/feature".to_owned(),
    "refs/heads/main".to_owned(),
  ];
  let (matched, entry) = idx
    .lookup(&read_ladder, version, "build-cache", &[])?
    .ok_or("expected a hit via the ladder's main scope")?;
  assert_eq!(matched, "build-cache");
  assert_eq!(entry.key, "build-cache");
  Ok(())
}

#[test]
fn exact_key_beats_restore_prefix() -> TestResult<()> {
  let dir = tempfile::tempdir()?;
  let idx = CacheIndex::new(dir.path().to_path_buf());
  let scope = "refs/heads/main";
  let version = "v1";
  // Exact target is OLDER than a prefix-matching sibling — exact must still win.
  idx.insert(scope, version, &mk_entry("deps-exact", 100)?)?;
  idx.insert(scope, version, &mk_entry("deps-newer", 200)?)?;
  let (matched, _) = idx
    .lookup(&ladder(scope), version, "deps-exact", &["deps-".to_owned()])?
    .ok_or("expected an exact-key hit")?;
  assert_eq!(
    matched, "deps-exact",
    "an exact key must win over a newer restore-key prefix"
  );
  Ok(())
}

#[test]
fn restore_prefix_returns_newest() -> TestResult<()> {
  let dir = tempfile::tempdir()?;
  let idx = CacheIndex::new(dir.path().to_path_buf());
  let scope = "refs/heads/main";
  let version = "v1";
  idx.insert(scope, version, &mk_entry("deps-abc", 1_000)?)?; // older
  idx.insert(scope, version, &mk_entry("deps-def", 2_000)?)?; // newer
  let (matched, _) = idx
    .lookup(&ladder(scope), version, "nomatch", &["deps-".to_owned()])?
    .ok_or("expected a restore-key prefix hit")?;
  assert_eq!(matched, "deps-def", "the newest prefix match must win");
  Ok(())
}

#[test]
fn nearest_scope_wins_over_a_newer_distant_scope() -> TestResult<()> {
  let dir = tempfile::tempdir()?;
  let idx = CacheIndex::new(dir.path().to_path_buf());
  let version = "v1";
  // The SAME key lives in both scopes; main's entry is NEWER than feature's.
  idx.insert("refs/heads/feature", version, &mk_entry("shared", 100)?)?;
  idx.insert("refs/heads/main", version, &mk_entry("shared", 999)?)?;
  let read_ladder = vec![
    "refs/heads/feature".to_owned(),
    "refs/heads/main".to_owned(),
  ];
  let (matched, entry) = idx
    .lookup(&read_ladder, version, "shared", &[])?
    .ok_or("expected a hit across the ladder")?;
  assert_eq!(matched, "shared");
  assert_eq!(
    entry.created_at.timestamp(),
    100,
    "nearest scope (feature) must win, not the globally-newest main entry"
  );
  Ok(())
}

#[test]
fn empty_restore_key_is_skipped() -> TestResult<()> {
  let dir = tempfile::tempdir()?;
  let idx = CacheIndex::new(dir.path().to_path_buf());
  let scope = "refs/heads/main";
  let version = "v1";
  idx.insert(scope, version, &mk_entry("anything", 1_000)?)?;
  // A `""` restore key would prefix-match everything; it must be ignored.
  let hit = idx.lookup(&ladder(scope), version, "nomatch", &["".to_owned()])?;
  assert!(
    hit.is_none(),
    "an empty restore key must not match an arbitrary entry"
  );
  Ok(())
}

#[test]
fn version_is_isolated() -> TestResult<()> {
  let dir = tempfile::tempdir()?;
  let idx = CacheIndex::new(dir.path().to_path_buf());
  let scope = "refs/heads/main";
  idx.insert(scope, "version-A", &mk_entry("shared-key", 1_000)?)?;
  let hit = idx.lookup(&ladder(scope), "version-B", "shared-key", &[])?;
  assert!(
    hit.is_none(),
    "an entry under version A must never resolve under version B"
  );
  Ok(())
}

#[test]
fn torn_tail_keeps_last_complete_entry() -> TestResult<()> {
  let dir = tempfile::tempdir()?;
  let idx = CacheIndex::new(dir.path().to_path_buf());
  let scope = "refs/heads/main";
  let version = "v1";
  idx.insert(scope, version, &mk_entry("torn-a", 1_000)?)?;
  idx.insert(scope, version, &mk_entry("torn-b", 2_000)?)?;
  // Simulate a crash mid-append: a partial line with no trailing newline.
  let jsonl = find_jsonl(dir.path())?.ok_or("no jsonl log was written")?;
  let mut file = fs::OpenOptions::new().append(true).open(&jsonl)?;
  file.write_all(b"{\"key\":\"torn-c\",\"manifest\":\"deadbeef")?;
  drop(file);
  let (matched, _) = idx
    .lookup(&ladder(scope), version, "nomatch", &["torn-".to_owned()])?
    .ok_or("expected the last complete entry despite the torn tail")?;
  assert_eq!(
    matched, "torn-b",
    "a torn trailing line must not shadow the last complete entry"
  );
  Ok(())
}

#[test]
fn survives_a_fresh_index_on_the_same_root() -> TestResult<()> {
  let dir = tempfile::tempdir()?;
  let root = dir.path().to_path_buf();
  let scope = "refs/heads/main";
  let version = "v1";
  {
    let idx = CacheIndex::new(root.clone());
    idx.insert(scope, version, &mk_entry("persist-key", 1_000)?)?;
  }
  // A brand-new index over the same root must still resolve the entry.
  let reopened = CacheIndex::new(root);
  let hit = reopened.lookup(&ladder(scope), version, "persist-key", &[])?;
  assert!(
    hit.is_some(),
    "entry must survive a new CacheIndex on the same root"
  );
  Ok(())
}

#[test]
fn entry_id_is_deterministic_and_js_safe() -> TestResult<()> {
  // Take real manifest digests off constructed entries (exercises `?`).
  let id = mk_entry("a-manifest-digest", 0)?.manifest;
  let first = entry_id_for(&id);
  let second = entry_id_for(&id);
  assert_eq!(
    first, second,
    "entry_id must be deterministic for one digest"
  );
  assert!(first < (1u64 << 53), "entry_id must fit a JS-safe integer");
  let other = entry_id_for(&mk_entry("a-different-manifest-digest", 0)?.manifest);
  assert_ne!(
    first, other,
    "distinct digests should yield distinct entry ids"
  );
  Ok(())
}
