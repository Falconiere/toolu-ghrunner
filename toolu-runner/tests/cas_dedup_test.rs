//! AC-9: measure how much the content-addressed store dedups when it chunks a
//! *compressed* archive verbatim (the way `actions/cache` uploads it).
//!
//! The premise from the spec's Non-Goal 3: a one-file change to a source tree,
//! re-tarred and re-compressed, rewrites most of the compressed byte stream, so
//! content-defined chunking of the verbatim archive finds almost no shared
//! chunks. This test ASSERTS that (>80% of the second archive is new bytes) and
//! records the measured ratio. If it ever fails, verbatim chunking dedups better
//! than expected and archive normalization (Non-Goal 3) is unnecessary — either
//! way the number drives the decision.
//!
//! Real data, no mocks: the archive is a real gzip of this repo's `shared/src`.

use std::collections::HashSet;
use std::io::Write;
use std::path::Path;

use toolu_runner::execution::cache::cas::{CasStore, ChunkId, Manifest};

/// Boxed error alias for test helpers that use `?`.
type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

/// One repo file: its archive path and current bytes.
struct RepoFile {
  name: String,
  bytes: Vec<u8>,
}

/// Recursively collect `shared/src` files, sorted by path for determinism.
fn collect_files(root: &Path) -> TestResult<Vec<RepoFile>> {
  let mut out = Vec::new();
  let mut stack = vec![root.to_path_buf()];
  while let Some(dir) = stack.pop() {
    for entry in std::fs::read_dir(&dir)? {
      let path = entry?.path();
      if path.is_dir() {
        stack.push(path);
      } else if path.is_file() {
        let name = path.strip_prefix(root)?.to_string_lossy().into_owned();
        out.push(RepoFile {
          name,
          bytes: std::fs::read(&path)?,
        });
      }
    }
  }
  out.sort_by(|a, b| a.name.cmp(&b.name));
  Ok(out)
}

/// Tar the files (in order) and gzip the tar, returning the compressed bytes.
fn tar_gz(files: &[RepoFile]) -> TestResult<Vec<u8>> {
  let mut tar = tar::Builder::new(Vec::new());
  for file in files {
    let mut header = tar::Header::new_gnu();
    header.set_size(u64::try_from(file.bytes.len())?);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append_data(&mut header, &file.name, file.bytes.as_slice())?;
  }
  let tar_bytes = tar.into_inner()?;
  let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
  gz.write_all(&tar_bytes)?;
  Ok(gz.finish()?)
}

/// Ingest `archive` bytes through a staging file and return the manifest.
async fn ingest_bytes(store: &CasStore, staging: &Path, archive: &[u8]) -> TestResult<Manifest> {
  std::fs::write(staging, archive)?;
  Ok(store.ingest(staging).await?)
}

/// Distinct ids in `manifest`.
fn chunk_id_set(manifest: &Manifest) -> HashSet<ChunkId> {
  manifest.chunks.iter().map(|c| c.id.clone()).collect()
}

/// Bytes in `later` whose chunk id did not appear in `earlier` (distinct ids only).
fn unique_bytes(later: &Manifest, earlier: &HashSet<ChunkId>) -> u64 {
  let mut counted = HashSet::new();
  let mut total: u64 = 0;
  for chunk in &later.chunks {
    if earlier.contains(&chunk.id) || !counted.insert(chunk.id.clone()) {
      continue;
    }
    total = total.saturating_add(u64::from(chunk.len));
  }
  total
}

/// Flip a byte in the largest file so the re-compressed archive diverges.
fn mutate_largest(files: &mut [RepoFile]) -> TestResult<()> {
  let target = files
    .iter_mut()
    .max_by_key(|f| f.bytes.len())
    .ok_or("no files collected")?;
  let byte = target.bytes.first_mut().ok_or("largest file is empty")?;
  *byte = byte.wrapping_add(1);
  Ok(())
}

#[tokio::test]
async fn verbatim_chunking_of_a_compressed_archive_barely_dedups() -> TestResult<()> {
  let dir = tempfile::tempdir()?;
  let store = CasStore::new(dir.path().join("cas"), 16384, 1 << 30);
  let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../shared/src");

  let mut files = collect_files(&src)?;
  let archive_a = tar_gz(&files)?;
  let manifest_a = ingest_bytes(&store, &dir.path().join("a.tgz"), &archive_a).await?;
  let ids_a = chunk_id_set(&manifest_a);

  mutate_largest(&mut files)?;
  let archive_b = tar_gz(&files)?;
  let manifest_b = ingest_bytes(&store, &dir.path().join("b.tgz"), &archive_b).await?;

  let new_bytes = unique_bytes(&manifest_b, &ids_a);
  let total_b = manifest_b.total_size;

  // Record the measured ratio (visible with --nocapture) so the decision is auditable.
  let pct = new_bytes.saturating_mul(100) / total_b.max(1);
  eprintln!(
    "AC-9: second archive {total_b} bytes, {new_bytes} new after a one-byte source change ({pct}% unique)"
  );

  // The decision rule: >80% unique means verbatim chunking does NOT dedup a
  // compressed archive, so tarball normalization (Non-Goal 3) would be required
  // to win real dedup. If this ever fails, the premise is wrong — revisit the
  // non-goal. Integer form of `new_bytes / total_b > 0.80`.
  assert!(
    new_bytes.saturating_mul(5) > total_b.saturating_mul(4),
    "expected >80% unique bytes (compression defeats CDC); got {pct}% — \
     verbatim chunking dedups better than assumed, revisit Non-Goal 3"
  );
  Ok(())
}
