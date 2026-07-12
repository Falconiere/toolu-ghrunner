//! Real-data L2 (S3) cold-tier test.
//!
//! Runs against a real MinIO/S3 endpoint when `TOOLU_TEST_S3_ENDPOINT` is set,
//! and SKIPS LOUDLY (printing a hint, returning green) when it is not — so the
//! step passes in CI without S3 and is genuinely exercised with it. No mocks.

use std::path::Path;

use futures_util::StreamExt;
use shared::L2Config;
use cache::cas::{CasStore, Manifest};
use cache::tier::{BlobKind, L2Tier};

/// Boxed error alias for test helpers that use `?`.
type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

/// The L2 test config from env, or `None` when `TOOLU_TEST_S3_ENDPOINT` is unset.
fn l2_config_from_env() -> Option<L2Config> {
  let endpoint = std::env::var("TOOLU_TEST_S3_ENDPOINT").ok()?;
  let bucket =
    std::env::var("TOOLU_TEST_S3_BUCKET").unwrap_or_else(|_| "toolu-cache-test".to_owned());
  let region = std::env::var("TOOLU_TEST_S3_REGION").unwrap_or_else(|_| "us-east-1".to_owned());
  Some(L2Config {
    bucket,
    endpoint,
    region,
  })
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

/// Recursively delete every regular file under `dir`, leaving directories; returns the count.
fn delete_all_files(dir: &Path) -> TestResult<usize> {
  if !dir.exists() {
    return Ok(0);
  }
  let mut removed = 0;
  for entry in std::fs::read_dir(dir)? {
    let path = entry?.path();
    if path.is_dir() {
      removed += delete_all_files(&path)?;
    } else {
      std::fs::remove_file(&path)?;
      removed += 1;
    }
  }
  Ok(removed)
}

#[tokio::test]
async fn l2_mirrors_on_ingest_and_restores_a_cold_l1() -> TestResult<()> {
  let Some(cfg) = l2_config_from_env() else {
    eprintln!(
      "SKIP: set TOOLU_TEST_S3_ENDPOINT (plus TOOLU_TEST_S3_BUCKET, AWS_ACCESS_KEY_ID, \
       AWS_SECRET_ACCESS_KEY) to run the L2 test against a real MinIO/S3"
    );
    return Ok(());
  };

  let dir = tempfile::tempdir()?;
  let root = dir.path().join("cas");
  let l2 = L2Tier::from_config(&cfg)?;
  let store = CasStore::new(root.clone(), 16384, 1 << 30).with_l2(Some(l2.clone()));

  // Real payload: this crate's own Cargo.lock.
  let lock = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock");
  let original = std::fs::read(&lock)?;
  assert!(!original.is_empty(), "Cargo.lock should be non-empty");

  let m = store.ingest(&lock).await?;
  assert_eq!(m.total_size, u64::try_from(original.len())?);

  // Ingest mirrored every chunk to the bucket.
  for chunk in &m.chunks {
    let hex = chunk.id.to_hex();
    assert!(
      l2.has_blob(BlobKind::Chunk, &hex).await?,
      "chunk {hex} was not mirrored to L2"
    );
  }

  // Make L1 cold: delete every blob file on disk.
  let blobs = root.join("blobs");
  let removed = delete_all_files(&blobs)?;
  assert!(removed > 0, "expected L1 blob files to delete");

  // read_range must restore the missing chunks through L2 and reproduce the bytes.
  let got = collect_range(&store, &m, 0, m.total_size).await?;
  assert_eq!(got, original, "L2 restore did not reproduce Cargo.lock");
  Ok(())
}
