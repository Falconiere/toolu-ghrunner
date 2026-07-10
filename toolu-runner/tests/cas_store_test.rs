//! Real-data CAS store tests: tar `shared/src`, ingest, read back, dedup, corruption.

use std::path::{Path, PathBuf};

use futures_util::StreamExt;
use toolu_runner::execution::cache::cas::{CasStore, Manifest};

/// Boxed error alias for test helpers that use `?`.
type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

/// A ready store plus the exact tar bytes it will ingest.
struct Fixture {
  _dir: tempfile::TempDir,
  root: PathBuf,
  tar_path: PathBuf,
  original: Vec<u8>,
  store: CasStore,
}

/// Build a real tarball of this repo's `shared/src` and a store rooted in a tempdir.
fn setup() -> TestResult<Fixture> {
  let dir = tempfile::tempdir()?;
  let root = dir.path().join("cas");
  let tar_path = dir.path().join("shared-src.tar");
  let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../shared/src");
  let original = build_tar(&tar_path, &src)?;
  let store = CasStore::new(root.clone(), 16384, 1 << 30);
  Ok(Fixture {
    _dir: dir,
    root,
    tar_path,
    original,
    store,
  })
}

/// Tar `src` into `dest` and return the on-disk tar bytes.
fn build_tar(dest: &Path, src: &Path) -> TestResult<Vec<u8>> {
  let mut builder = tar::Builder::new(std::fs::File::create(dest)?);
  builder.append_dir_all("shared-src", src)?;
  builder.into_inner()?;
  Ok(std::fs::read(dest)?)
}

/// Collect a ranged read into one buffer, propagating any verify error.
async fn collect_range(store: &CasStore, m: &Manifest, off: u64, len: u64) -> TestResult<Vec<u8>> {
  let stream = store.read_range(m, off, len);
  futures_util::pin_mut!(stream);
  let mut out = Vec::new();
  while let Some(item) = stream.next().await {
    out.extend_from_slice(&item?);
  }
  Ok(out)
}

/// Count regular files under `dir`, recursing into subdirs.
fn count_files(dir: &Path) -> TestResult<usize> {
  if !dir.exists() {
    return Ok(0);
  }
  let mut n = 0;
  for entry in std::fs::read_dir(dir)? {
    let path = entry?.path();
    if path.is_dir() {
      n += count_files(&path)?;
    } else {
      n += 1;
    }
  }
  Ok(n)
}

/// The first regular file found under `dir`, or `None`.
fn first_file(dir: &Path) -> TestResult<Option<PathBuf>> {
  for entry in std::fs::read_dir(dir)? {
    let path = entry?.path();
    if path.is_dir() {
      if let Some(found) = first_file(&path)? {
        return Ok(Some(found));
      }
    } else {
      return Ok(Some(path));
    }
  }
  Ok(None)
}

#[tokio::test]
async fn ingest_reads_back_byte_for_byte() -> TestResult<()> {
  let fx = setup()?;
  assert!(
    !fx.original.is_empty(),
    "tar of shared/src should be non-empty"
  );
  let m = fx.store.ingest(&fx.tar_path).await?;
  assert_eq!(m.total_size, u64::try_from(fx.original.len())?);
  let got = collect_range(&fx.store, &m, 0, m.total_size).await?;
  assert_eq!(
    got, fx.original,
    "read_range did not reproduce the tar bytes"
  );
  Ok(())
}

#[tokio::test]
async fn dedup_second_ingest_writes_no_new_chunks() -> TestResult<()> {
  let fx = setup()?;
  let blobs = fx.root.join("blobs");
  let m1 = fx.store.ingest(&fx.tar_path).await?;
  let before = count_files(&blobs)?;
  assert!(before > 0, "first ingest should write at least one chunk");
  let m2 = fx.store.ingest(&fx.tar_path).await?;
  let after = count_files(&blobs)?;
  assert_eq!(before, after, "identical re-ingest wrote new chunk files");
  assert_eq!(m1, m2, "identical input produced a different manifest");
  Ok(())
}

#[tokio::test]
async fn corruption_is_detected_on_read() -> TestResult<()> {
  let fx = setup()?;
  let blobs = fx.root.join("blobs");
  let m = fx.store.ingest(&fx.tar_path).await?;
  let victim = first_file(&blobs)?.ok_or("no chunk file was written")?;
  let mut bytes = std::fs::read(&victim)?;
  let first = bytes.get_mut(0).ok_or("chunk file was empty")?;
  *first ^= 0xff;
  std::fs::write(&victim, &bytes)?;
  let stream = fx.store.read_range(&m, 0, m.total_size);
  futures_util::pin_mut!(stream);
  let mut saw_err = false;
  while let Some(item) = stream.next().await {
    if item.is_err() {
      saw_err = true;
      break;
    }
  }
  assert!(
    saw_err,
    "flipped chunk byte was not caught by BLAKE3 verify"
  );
  Ok(())
}
