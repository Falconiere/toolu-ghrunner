//! The content-addressed store: FastCDC ingest, ranged streamed reads, manifest persistence.

use std::fs;
use std::path::{Path, PathBuf};

use futures_util::Stream;
use shared::RunnerError;

use super::chunk_io;
use super::chunker;
use super::manifest::{ChunkId, Manifest};
use crate::execution::cache::tier::{BlobKind, L2Tier};

/// One ranged read step: chunk id, bytes to skip within it, bytes to take.
type ReadStep = (ChunkId, u64, u64);

/// Content-addressed chunk store rooted at a local directory.
///
/// `Clone` yields a second handle to the *same* on-disk store (it copies only
/// the root path and size parameters), so the Twirp and blob routers can share
/// one store.
#[derive(Clone)]
pub struct CasStore {
  root: PathBuf,
  chunk_avg_bytes: u32,
  max_bytes: u64,
  l2: Option<L2Tier>,
}

impl CasStore {
  /// Create a store rooted at `root` with the given FastCDC average and L1 byte cap.
  ///
  /// The optional S3 cold tier defaults to `None`; attach one with [`with_l2`](Self::with_l2).
  pub fn new(root: PathBuf, chunk_avg_bytes: u32, max_bytes: u64) -> Self {
    Self {
      root,
      chunk_avg_bytes,
      max_bytes,
      l2: None,
    }
  }

  /// Attach (or clear) the optional S3 cold tier, returning the store for chaining.
  #[must_use]
  pub fn with_l2(mut self, l2: Option<L2Tier>) -> Self {
    self.l2 = l2;
    self
  }

  /// The configured L1 byte cap (consumed by GC in a later step).
  pub fn max_bytes(&self) -> u64 {
    self.max_bytes
  }

  /// Directory holding content-addressed chunk blobs.
  fn blobs_dir(&self) -> PathBuf {
    self.root.join("blobs")
  }

  /// Directory holding content-addressed manifests.
  fn manifests_dir(&self) -> PathBuf {
    self.root.join("manifests")
  }

  /// Chunk an assembled staging file with FastCDC, writing each unique chunk; returns the manifest.
  ///
  /// # Errors
  /// `RunnerError::Cache`/`Io` if the file cannot be read or a chunk write fails.
  pub async fn ingest(&self, staged: &Path) -> Result<Manifest, RunnerError> {
    let blobs = self.blobs_dir();
    let staged = staged.to_path_buf();
    let avg = self.chunk_avg_bytes;
    let manifest =
      tokio::task::spawn_blocking(move || chunker::chunk_and_store(&staged, &blobs, avg))
        .await
        .map_err(|e| RunnerError::Cache(format!("ingest task join failed: {e}")))??;
    self.mirror_chunks_to_l2(&manifest).await;
    Ok(manifest)
  }

  /// Persist a manifest content-addressed (BLAKE3 of its JSON); returns its id.
  ///
  /// # Errors
  /// `RunnerError::Json`/`Cache` if serialization or the on-disk write fails.
  pub async fn put_manifest(&self, m: &Manifest) -> Result<ChunkId, RunnerError> {
    let json = serde_json::to_vec(m).map_err(RunnerError::Json)?;
    let id = ChunkId(*blake3::hash(&json).as_bytes());
    let hex = id.to_hex();
    let path = chunk_io::blob_path(&self.manifests_dir(), &hex);
    let for_write = json.clone();
    tokio::task::spawn_blocking(move || chunk_io::write_atomic_sync(&path, &for_write))
      .await
      .map_err(|e| RunnerError::Cache(format!("manifest write join failed: {e}")))??;
    if let Some(l2) = &self.l2
      && let Err(e) = l2.put_blob(BlobKind::Manifest, &hex, &json).await
    {
      tracing::warn!(error = %e, "L2 manifest mirror failed; L1 unaffected");
    }
    Ok(id)
  }

  /// Load a manifest by id, verifying the stored JSON's BLAKE3 matches `id`.
  ///
  /// # Errors
  /// `RunnerError::Cache`/`Json` on digest mismatch, missing file, or bad JSON.
  pub async fn get_manifest(&self, id: &ChunkId) -> Result<Manifest, RunnerError> {
    let path = chunk_io::blob_path(&self.manifests_dir(), &id.to_hex());
    let bytes = chunk_io::read_verified(&path, id).await?;
    serde_json::from_slice(&bytes).map_err(RunnerError::Json)
  }

  /// Streamed ranged read; each chunk is BLAKE3-verified and never buffers the whole range.
  ///
  /// The returned stream owns its read plan and blob directory, so it is
  /// `'static` and can back an HTTP response body outliving this borrow.
  pub fn read_range(
    &self,
    m: &Manifest,
    offset: u64,
    len: u64,
  ) -> impl Stream<Item = Result<Vec<u8>, RunnerError>> + Send + 'static {
    let plan = build_plan(m, offset, len);
    let blobs = self.blobs_dir();
    let l2 = self.l2.clone();
    futures_util::stream::unfold(
      (plan.into_iter(), blobs, l2),
      |(mut steps, blobs, l2)| async move {
        let (id, skip, take) = steps.next()?;
        let item = read_slice(&blobs, l2.as_ref(), &id, skip, take).await;
        Some((item, (steps, blobs, l2)))
      },
    )
  }

  /// Best-effort mirror of every chunk in `m` to L2; a single WARN on the first failure.
  ///
  /// An absent or unreachable L2 never fails ingest — L1 is already durable.
  async fn mirror_chunks_to_l2(&self, m: &Manifest) {
    let Some(l2) = &self.l2 else {
      return;
    };
    let blobs = self.blobs_dir();
    for chunk in &m.chunks {
      let hex = chunk.id.to_hex();
      let path = chunk_io::blob_path(&blobs, &hex);
      let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(e) => {
          tracing::warn!(error = %e, "L2 mirror: reading L1 chunk failed; L1 unaffected");
          return;
        },
      };
      if let Err(e) = l2.put_blob(BlobKind::Chunk, &hex, &bytes).await {
        tracing::warn!(error = %e, "L2 chunk mirror failed; L1 unaffected");
        return;
      }
    }
  }

  /// True if a chunk with `id` already exists on disk.
  pub async fn has_chunk(&self, id: &ChunkId) -> bool {
    let path = chunk_io::blob_path(&self.blobs_dir(), &id.to_hex());
    tokio::fs::try_exists(&path).await.unwrap_or(false)
  }

  /// Enumerate every content-addressed chunk id currently on disk.
  ///
  /// # Errors
  /// `RunnerError::Io` if a shard directory cannot be read.
  pub fn list_chunk_ids(&self) -> Result<Vec<ChunkId>, RunnerError> {
    list_ids_in(&self.blobs_dir())
  }

  /// Enumerate every content-addressed manifest id currently on disk.
  ///
  /// # Errors
  /// `RunnerError::Io` if a shard directory cannot be read.
  pub fn list_manifest_ids(&self) -> Result<Vec<ChunkId>, RunnerError> {
    list_ids_in(&self.manifests_dir())
  }

  /// Delete a chunk blob by id; a missing blob is not an error.
  ///
  /// # Errors
  /// `RunnerError::Io` if the file exists but cannot be removed.
  pub async fn delete_chunk(&self, id: &ChunkId) -> Result<(), RunnerError> {
    remove_blob(&self.blobs_dir(), id).await
  }

  /// Delete a manifest blob by id; a missing blob is not an error.
  ///
  /// # Errors
  /// `RunnerError::Io` if the file exists but cannot be removed.
  pub async fn delete_manifest(&self, id: &ChunkId) -> Result<(), RunnerError> {
    remove_blob(&self.manifests_dir(), id).await
  }
}

/// Enumerate the ids under a sharded `<base>/<hex[..2]>/<hex>` tree, skipping temp files.
fn list_ids_in(base: &Path) -> Result<Vec<ChunkId>, RunnerError> {
  let shards = match fs::read_dir(base) {
    Ok(shards) => shards,
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
    Err(e) => return Err(RunnerError::Io(e)),
  };
  let mut out = Vec::new();
  for shard in shards {
    let shard = shard.map_err(RunnerError::Io)?.path();
    if !shard.is_dir() {
      continue;
    }
    for file in fs::read_dir(&shard).map_err(RunnerError::Io)? {
      let name = file.map_err(RunnerError::Io)?.file_name();
      if let Some(id) = name.to_str().and_then(|hex| ChunkId::from_hex(hex).ok()) {
        out.push(id);
      }
    }
  }
  Ok(out)
}

/// Remove one content-addressed blob under `base`; a missing file is a no-op.
async fn remove_blob(base: &Path, id: &ChunkId) -> Result<(), RunnerError> {
  let path = chunk_io::blob_path(base, &id.to_hex());
  match tokio::fs::remove_file(&path).await {
    Ok(()) => Ok(()),
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
    Err(e) => Err(RunnerError::Io(e)),
  }
}

/// Build the owned read plan (chunk id, skip, take) covering `[offset, offset+len)`.
fn build_plan(m: &Manifest, offset: u64, len: u64) -> Vec<ReadStep> {
  let mut plan = Vec::new();
  let Some((start_idx, intra)) = m.locate(offset) else {
    return plan;
  };
  let mut remaining = len.min(m.total_size.saturating_sub(offset));
  let mut skip = intra;
  for chunk in m.chunks.iter().skip(start_idx) {
    if remaining == 0 {
      break;
    }
    let take = u64::from(chunk.len).saturating_sub(skip).min(remaining);
    plan.push((chunk.id.clone(), skip, take));
    remaining = remaining.saturating_sub(take);
    skip = 0;
  }
  plan
}

/// Read one chunk (restoring from L2 into L1 if absent), verify it, return the intra-chunk slice.
async fn read_slice(
  blobs: &Path,
  l2: Option<&L2Tier>,
  id: &ChunkId,
  skip: u64,
  take: u64,
) -> Result<Vec<u8>, RunnerError> {
  let path = chunk_io::blob_path(blobs, &id.to_hex());
  let bytes = read_chunk_or_restore(&path, l2, id).await?;
  slice_of(&bytes, skip, take)
}

/// Read+verify a chunk from L1; if absent and L2 is present, restore it into L1 first.
async fn read_chunk_or_restore(
  path: &Path,
  l2: Option<&L2Tier>,
  id: &ChunkId,
) -> Result<Vec<u8>, RunnerError> {
  match chunk_io::read_verified(path, id).await {
    Ok(bytes) => Ok(bytes),
    Err(RunnerError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
      restore_from_l2(path, l2, id).await
    },
    Err(e) => Err(e),
  }
}

/// Pull a chunk from L2 into L1 (crash-safe), verifying before the write so bad data never poisons L1.
async fn restore_from_l2(
  path: &Path,
  l2: Option<&L2Tier>,
  id: &ChunkId,
) -> Result<Vec<u8>, RunnerError> {
  let hex = id.to_hex();
  let Some(l2) = l2 else {
    return Err(RunnerError::Cache(format!("missing chunk {hex}")));
  };
  let Some(bytes) = l2.get_blob(BlobKind::Chunk, &hex).await? else {
    return Err(RunnerError::Cache(format!(
      "missing chunk {hex} (absent from L1 and L2)"
    )));
  };
  chunk_io::verify_bytes(&bytes, id)?;
  let dest = path.to_path_buf();
  let for_write = bytes.clone();
  tokio::task::spawn_blocking(move || chunk_io::write_atomic_sync(&dest, &for_write))
    .await
    .map_err(|e| RunnerError::Cache(format!("L2 restore write join failed: {e}")))??;
  Ok(bytes)
}

/// Return `[skip, skip+take)` of `bytes` as an owned vec.
fn slice_of(bytes: &[u8], skip: u64, take: u64) -> Result<Vec<u8>, RunnerError> {
  let start =
    usize::try_from(skip).map_err(|e| RunnerError::Cache(format!("offset overflow: {e}")))?;
  let end = start.saturating_add(
    usize::try_from(take).map_err(|e| RunnerError::Cache(format!("len overflow: {e}")))?,
  );
  let slice = bytes
    .get(start..end)
    .ok_or_else(|| RunnerError::Cache("chunk slice out of range".into()))?;
  Ok(slice.to_vec())
}
