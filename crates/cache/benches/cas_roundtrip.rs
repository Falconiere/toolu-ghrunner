#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::print_stdout, clippy::cast_precision_loss)]
//! Save + restore throughput of the accelerated cache's content-addressed
//! store over a real directory of at least 500 MB (#112).
//!
//! Not a criterion bench: one pass over half a gigabyte is the measurement,
//! and criterion's sample loop would multiply it. The bench builds a
//! directory of files (a mix of incompressible and repetitive content, so
//! `FastCDC` sees both fresh chunks and dedup hits), assembles them into the
//! staged blob a cache save hands to [`CasStore::ingest`], then restores every
//! byte through [`CasStore::read_range`] (BLAKE3-verified per chunk) and checks
//! the restored length and hash against the source. Prints `save`/`restore`
//! wall time and MB/s on stdout.
//!
//! `CAS_BENCH_MB` overrides the 512 MB default (smoke runs only; the
//! acceptance measurement uses the default).

use std::io::Write;
use std::path::Path;
use std::time::Instant;

use cache::cas::store::CasStore;
use futures_util::StreamExt;

/// Default payload size in MiB.
const DEFAULT_MB: u64 = 512;
/// Files the payload is split into.
const FILE_COUNT: u64 = 64;
/// `FastCDC` average chunk size, the production default (1 MiB).
const CHUNK_AVG: u32 = 1 << 20;

/// Deterministic xorshift64 byte stream: incompressible and reproducible.
fn fill_random(seed: u64, buf: &mut [u8]) {
  let mut s = seed | 1;
  for b in buf.iter_mut() {
    s ^= s << 13;
    s ^= s >> 7;
    s ^= s << 17;
    *b = s.to_le_bytes()[0];
  }
}

/// Write `FILE_COUNT` files totalling `total` bytes; every fourth file is a
/// repeated block so the store sees duplicate chunks.
fn build_dir(dir: &Path, total: u64) -> u64 {
  let per_file = usize::try_from(total / FILE_COUNT).unwrap();
  let mut buf = vec![0u8; per_file];
  let mut written = 0;
  for i in 0..FILE_COUNT {
    if i % 4 == 3 {
      let block = vec![b'a' + u8::try_from(i % 26).unwrap(); 64 * 1024];
      for chunk in buf.chunks_mut(block.len()) {
        chunk.copy_from_slice(&block[..chunk.len()]);
      }
    } else {
      fill_random(i + 1, &mut buf);
    }
    std::fs::write(dir.join(format!("file-{i:03}.bin")), &buf).unwrap();
    written += buf.len() as u64;
  }
  written
}

/// Concatenate the directory's files, in name order, into `staged`.
fn assemble(dir: &Path, staged: &Path) -> blake3::Hash {
  let mut names: Vec<_> = std::fs::read_dir(dir)
    .unwrap()
    .map(|e| e.unwrap().path())
    .collect();
  names.sort();
  let mut out = std::io::BufWriter::new(std::fs::File::create(staged).unwrap());
  let mut hasher = blake3::Hasher::new();
  for p in names {
    let bytes = std::fs::read(p).unwrap();
    hasher.update(&bytes);
    out.write_all(&bytes).unwrap();
  }
  out.flush().unwrap();
  hasher.finalize()
}

/// MB/s for `bytes` over `secs`.
fn mbps(bytes: u64, secs: f64) -> f64 {
  bytes as f64 / 1_000_000.0 / secs
}

fn main() {
  let mb = std::env::var("CAS_BENCH_MB")
    .ok()
    .and_then(|v| v.parse().ok())
    .unwrap_or(DEFAULT_MB);
  let tmp = tempfile::tempdir().unwrap();
  let src = tmp.path().join("src");
  std::fs::create_dir(&src).unwrap();
  let total = build_dir(&src, mb * 1024 * 1024);
  let staged = tmp.path().join("staged.bin");
  let want = assemble(&src, &staged);

  let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
  let store = CasStore::new(tmp.path().join("cas"), CHUNK_AVG, u64::MAX);

  let t = Instant::now();
  let manifest = rt.block_on(store.ingest(&staged)).unwrap();
  let save = t.elapsed().as_secs_f64();

  let t = Instant::now();
  let (got, len) = rt.block_on(async {
    let mut hasher = blake3::Hasher::new();
    let mut len = 0u64;
    let mut stream = Box::pin(store.read_range(&manifest, 0, total));
    while let Some(piece) = stream.next().await {
      let piece = piece.unwrap();
      len += piece.len() as u64;
      hasher.update(&piece);
    }
    (hasher.finalize(), len)
  });
  let restore = t.elapsed().as_secs_f64();

  assert_eq!(len, total, "restored length");
  assert_eq!(got, want, "restored bytes differ from the source");
  println!(
    "cas_roundtrip bytes={total} chunks={} save_s={save:.3} save_MBps={:.1} restore_s={restore:.3} restore_MBps={:.1}",
    manifest.chunks.len(),
    mbps(total, save),
    mbps(total, restore)
  );
}
