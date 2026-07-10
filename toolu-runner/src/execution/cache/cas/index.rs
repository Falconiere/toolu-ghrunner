//! Persistent, restart-safe cache index: append-only `(scope, version)`
//! JSONL logs mapping client keys to manifest pointers.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as SEGMENT;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use shared::RunnerError;

use super::manifest::ChunkId;

/// One index record: a client key pointing at a manifest blob, with size and age.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexEntry {
  /// The client-supplied, opaque cache key.
  pub key: String,
  /// BLAKE3 id of the manifest blob describing the cached archive.
  pub manifest: ChunkId,
  /// Total assembled size of the cached archive in bytes.
  pub size_bytes: u64,
  /// When this entry was written (newest wins within a match set).
  pub created_at: DateTime<Utc>,
}

/// One enumerated index record: its `(scope, version)` plus the entry itself.
#[derive(Debug, Clone)]
pub struct IndexRecord {
  /// The cache scope (branch ref) this entry lives under.
  pub scope: String,
  /// The exact cache version this entry lives under.
  pub version: String,
  /// The index entry itself.
  pub entry: IndexEntry,
}

/// Append-only cache index rooted at a local directory; reads nothing eagerly.
///
/// `Clone` yields a second handle to the *same* on-disk index (it copies only
/// the root path), since every read and write goes straight to disk.
#[derive(Clone)]
pub struct CacheIndex {
  root: PathBuf,
}

impl CacheIndex {
  /// Create an index rooted at `root`. Performs no I/O until `insert`/`lookup`.
  pub fn new(root: PathBuf) -> Self {
    Self { root }
  }

  /// The `.jsonl` log for one `(scope, version)`, each part a single safe segment.
  fn version_file(&self, scope: &str, version: &str) -> PathBuf {
    self
      .root
      .join("index")
      .join(SEGMENT.encode(scope.as_bytes()))
      .join(format!("{}.jsonl", SEGMENT.encode(version.as_bytes())))
  }

  /// Append one entry for `(scope, version)` as a single `O_APPEND` line write.
  ///
  /// # Errors
  /// `RunnerError::Io`/`Json` if the log cannot be created, serialized, or written.
  pub fn insert(&self, scope: &str, version: &str, entry: &IndexEntry) -> Result<(), RunnerError> {
    let path = self.version_file(scope, version);
    if let Some(parent) = path.parent() {
      fs::create_dir_all(parent).map_err(RunnerError::Io)?;
    }
    let mut line = serde_json::to_string(entry).map_err(RunnerError::Json)?;
    line.push('\n');
    let mut file = fs::OpenOptions::new()
      .create(true)
      .append(true)
      .open(&path)
      .map_err(RunnerError::Io)?;
    // One write_all of `line` (the `\n` was appended at L74) under O_APPEND —
    // never two writes.
    file.write_all(line.as_bytes()).map_err(RunnerError::Io)?;
    Ok(())
  }

  /// Resolve a key with nearest-scope-wins precedence over the read ladder.
  ///
  /// `version` is exact throughout (it selects the per-version log). The exact
  /// `key` is tried first: the earliest ladder scope holding it wins (newest
  /// `created_at` within that scope), so a job's own scope shadows a base or
  /// protected scope. Only if no scope has the exact key does each non-empty
  /// `restore_key` (in caller order) fall back to a prefix match, again nearest
  /// scope first. Empty restore keys are skipped (they would match anything).
  /// Reads fresh from disk, so it is restart-safe. Returns `(matched_key, entry)`.
  ///
  /// # Errors
  /// `RunnerError::Io` if a scope's log exists but cannot be read.
  pub fn lookup(
    &self,
    ladder: &[String],
    version: &str,
    key: &str,
    restore_keys: &[String],
  ) -> Result<Option<(String, IndexEntry)>, RunnerError> {
    let per_scope: Vec<Vec<IndexEntry>> = ladder
      .iter()
      .map(|scope| read_entries(&self.version_file(scope, version)))
      .collect::<Result<_, _>>()?;

    // Exact key: nearest scope wins, newest within that scope.
    for entries in &per_scope {
      if let Some(hit) = newest(entries.iter().filter(|e| e.key == key)) {
        return Ok(Some((hit.key.clone(), hit.clone())));
      }
    }
    // Restore keys: each in caller order, nearest scope with a prefix match wins.
    for restore_key in restore_keys.iter().filter(|k| !k.is_empty()) {
      for entries in &per_scope {
        if let Some(hit) = newest(
          entries
            .iter()
            .filter(|e| e.key.starts_with(restore_key.as_str())),
        ) {
          return Ok(Some((hit.key.clone(), hit.clone())));
        }
      }
    }
    Ok(None)
  }

  /// Enumerate every complete entry across all `(scope, version)` logs.
  ///
  /// The encoded filenames are decoded back to their original scope/version.
  ///
  /// # Errors
  /// `RunnerError::Io` if a directory or log cannot be read, or `Cache` if a
  /// filename segment cannot be decoded.
  pub fn records(&self) -> Result<Vec<IndexRecord>, RunnerError> {
    let read = match fs::read_dir(self.root.join("index")) {
      Ok(read) => read,
      Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
      Err(e) => return Err(RunnerError::Io(e)),
    };
    let mut out = Vec::new();
    for child in read {
      let child = child.map_err(RunnerError::Io)?;
      let path = child.path();
      if path.is_dir() {
        let scope = decode_segment(&child.file_name())?;
        collect_versions(&path, &scope, &mut out)?;
      }
    }
    Ok(out)
  }

  /// Atomically rewrite one `(scope, version)` log to exactly `entries`.
  ///
  /// An empty `entries` removes the log entirely. The write is a sibling temp
  /// file plus rename, so a reader never observes a partially written log.
  ///
  /// # Errors
  /// `RunnerError::Io`/`Json` if the log cannot be serialized, written, renamed,
  /// or removed.
  pub fn rewrite(
    &self,
    scope: &str,
    version: &str,
    entries: &[IndexEntry],
  ) -> Result<(), RunnerError> {
    let path = self.version_file(scope, version);
    if entries.is_empty() {
      return match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(RunnerError::Io(e)),
      };
    }
    let mut buf = String::new();
    for entry in entries {
      buf.push_str(&serde_json::to_string(entry).map_err(RunnerError::Json)?);
      buf.push('\n');
    }
    write_replace(&path, buf.as_bytes())
  }
}

/// Append every entry under one scope directory, decoding each version filename.
fn collect_versions(
  scope_path: &Path,
  scope: &str,
  out: &mut Vec<IndexRecord>,
) -> Result<(), RunnerError> {
  for child in fs::read_dir(scope_path).map_err(RunnerError::Io)? {
    let path = child.map_err(RunnerError::Io)?.path();
    if path.extension().is_some_and(|ext| ext == "jsonl") {
      let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| RunnerError::Cache("index filename not utf8".into()))?;
      let version = decode_segment_str(stem)?;
      for entry in read_entries(&path)? {
        out.push(IndexRecord {
          scope: scope.to_owned(),
          version: version.clone(),
          entry,
        });
      }
    }
  }
  Ok(())
}

/// Decode a base64url path-segment `OsStr` back to its original string.
fn decode_segment(name: &std::ffi::OsStr) -> Result<String, RunnerError> {
  let text = name
    .to_str()
    .ok_or_else(|| RunnerError::Cache("index segment not utf8".into()))?;
  decode_segment_str(text)
}

/// Decode a base64url path segment back to its original string.
fn decode_segment_str(text: &str) -> Result<String, RunnerError> {
  let bytes = SEGMENT
    .decode(text)
    .map_err(|e| RunnerError::Cache(format!("bad index segment: {e}")))?;
  String::from_utf8(bytes).map_err(|e| RunnerError::Cache(format!("index segment not utf8: {e}")))
}

/// Replace `path` atomically via a sibling temp file, fsync, and rename.
fn write_replace(path: &Path, bytes: &[u8]) -> Result<(), RunnerError> {
  let parent = path
    .parent()
    .ok_or_else(|| RunnerError::Cache("index path has no parent".into()))?;
  fs::create_dir_all(parent).map_err(RunnerError::Io)?;
  let mut name = path
    .file_name()
    .map(std::ffi::OsStr::to_os_string)
    .unwrap_or_default();
  name.push(".tmp.");
  name.push(uuid::Uuid::new_v4().to_string());
  let tmp = path.with_file_name(name);
  let mut file = fs::File::create(&tmp).map_err(RunnerError::Io)?;
  file.write_all(bytes).map_err(RunnerError::Io)?;
  file.sync_all().map_err(RunnerError::Io)?;
  match fs::rename(&tmp, path) {
    Ok(()) => Ok(()),
    Err(e) => {
      let _ = fs::remove_file(&tmp);
      Err(RunnerError::Io(e))
    },
  }
}

/// The newest entry (by `created_at`) an iterator yields, if any.
fn newest<'a>(entries: impl Iterator<Item = &'a IndexEntry>) -> Option<&'a IndexEntry> {
  entries.max_by_key(|e| e.created_at)
}

/// Read every complete JSONL entry from `path`, tolerating a torn trailing line.
///
/// A missing file yields no entries. The bytes after the last `\n` are a
/// crash-torn tail and are dropped; a line that fails to parse is skipped with
/// a WARN, never fatal (a lost entry is only a cache miss).
fn read_entries(path: &Path) -> Result<Vec<IndexEntry>, RunnerError> {
  let bytes = match fs::read(path) {
    Ok(bytes) => bytes,
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
    Err(e) => return Err(RunnerError::Io(e)),
  };
  let Some(last_nl) = bytes.iter().rposition(|&b| b == b'\n') else {
    return Ok(Vec::new());
  };
  // Keep bytes through the last newline; discard any partial trailing line.
  let complete = bytes.get(..last_nl.saturating_add(1)).unwrap_or(&[]);
  let text = String::from_utf8_lossy(complete);
  let mut out = Vec::new();
  for line in text.lines() {
    if line.is_empty() {
      continue;
    }
    match serde_json::from_str::<IndexEntry>(line) {
      Ok(entry) => out.push(entry),
      Err(err) => tracing::warn!(error = %err, "cache index: skipping unparseable line"),
    }
  }
  Ok(out)
}

/// The Twirp `entry_id`: the low 53 bits of the manifest digest, a JS-safe
/// integer (the toolkit does `parseInt(entryId)`), stable across restart.
pub fn entry_id_for(manifest: &ChunkId) -> u64 {
  let head = manifest.0.first_chunk::<8>().copied().unwrap_or([0u8; 8]);
  u64::from_be_bytes(head) & ((1u64 << 53) - 1)
}
