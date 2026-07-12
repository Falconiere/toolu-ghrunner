//! AC-9: measure how much the content-addressed store dedups when it chunks a
//! *compressed* archive verbatim (the way `actions/cache` uploads it).
//!
//! The premise from the spec's Non-Goal 3: a one-file change to a source tree,
//! re-tarred and re-compressed, rewrites the compressed byte stream from the
//! change point onward, so content-defined chunking of the verbatim archive
//! finds almost no shared chunks. This test ASSERTS that (>80% of the second
//! archive is new bytes) and records the measured ratio. If it ever fails,
//! verbatim chunking dedups better than expected and archive normalization
//! (Non-Goal 3) is unnecessary — either way the number drives the decision.
//!
//! Real data, no mocks: the corpus is deterministic seeded pseudo-source text
//! written to real files in a tempdir (not repo sources, so editing the repo
//! can never move the measured ratio), then tarred and gzipped for real. The
//! corpus must be LZ-compressible like a real source tree: on pure random
//! bytes deflate degenerates to Huffman-only coding, a one-byte flip leaves
//! the downstream bit stream aligned and identical, and CDC resyncs (measured
//! 4% unique) — which is NOT what `actions/cache` archives look like. The
//! one-file edit changes the file's LENGTH (as real edits do), shifting every
//! downstream tar block so every deflate block is re-cut over different
//! input. The edited file is the archive's FIRST, so the divergence spans
//! (almost) the whole second archive rather than depending on where some repo
//! file happens to sort. (The same edit to an UNCOMPRESSED tar would let CDC
//! resync right after the insertion — the near-total divergence measured here
//! is exactly compression's doing.)

use std::collections::HashSet;
use std::io::Write;
use std::path::Path;

use cache::cas::{CasStore, ChunkId, Manifest};

/// Boxed error alias for test helpers that use `?`.
type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

/// Number of generated corpus files.
const FILE_COUNT: usize = 24;
/// Distinct vocabulary words shared by every corpus file.
const VOCAB_WORDS: u64 = 512;
/// Words per generated corpus file (about 30 KiB of text each).
const WORDS_PER_FILE: usize = 4096;

/// One corpus file: its archive path and current bytes.
struct CorpusFile {
  name: String,
  bytes: Vec<u8>,
}

/// Deterministic xorshift64* PRNG, fix-seeded so every run on every platform
/// generates the identical corpus.
struct Xorshift64Star(u64);

impl Xorshift64Star {
  fn next_u64(&mut self) -> u64 {
    self.0 ^= self.0 >> 12;
    self.0 ^= self.0 << 25;
    self.0 ^= self.0 >> 27;
    self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
  }
}

/// The seeded shared vocabulary: `VOCAB_WORDS` lowercase words of 3–10 chars.
fn vocabulary(rng: &mut Xorshift64Star) -> TestResult<Vec<String>> {
  let mut vocab = Vec::new();
  for _ in 0..VOCAB_WORDS {
    let len = 3 + usize::try_from(rng.next_u64() % 8)?;
    let mut word = String::with_capacity(len);
    for _ in 0..len {
      let letter = u8::try_from(rng.next_u64() % 26)?.saturating_add(b'a');
      word.push(char::from(letter));
    }
    vocab.push(word);
  }
  Ok(vocab)
}

/// One pseudo-source file: seeded vocabulary words, eight per line. Repeated
/// words give deflate real LZ matches, like the source trees `actions/cache`
/// actually archives.
fn generate_file(rng: &mut Xorshift64Star, vocab: &[String]) -> TestResult<Vec<u8>> {
  let mut out = Vec::new();
  for i in 0..WORDS_PER_FILE {
    let idx = usize::try_from(rng.next_u64() % VOCAB_WORDS)?;
    let word = vocab.get(idx).ok_or("vocab index out of range")?;
    out.extend_from_slice(word.as_bytes());
    out.push(if (i + 1) % 8 == 0 { b'\n' } else { b' ' });
  }
  Ok(out)
}

/// Write the deterministic pseudo-source corpus as real files under `root`,
/// then collect them back from disk, sorted by path.
fn write_corpus(root: &Path) -> TestResult<Vec<CorpusFile>> {
  std::fs::create_dir_all(root)?;
  let mut rng = Xorshift64Star(0x9E37_79B9_7F4A_7C15);
  let vocab = vocabulary(&mut rng)?;
  for i in 0..FILE_COUNT {
    let bytes = generate_file(&mut rng, &vocab)?;
    std::fs::write(root.join(format!("file-{i:02}.txt")), &bytes)?;
  }
  collect_files(root)
}

/// Recursively collect the files under `root`, sorted by path for determinism.
fn collect_files(root: &Path) -> TestResult<Vec<CorpusFile>> {
  let mut out = Vec::new();
  let mut stack = vec![root.to_path_buf()];
  while let Some(dir) = stack.pop() {
    for entry in std::fs::read_dir(&dir)? {
      let path = entry?.path();
      if path.is_dir() {
        stack.push(path);
      } else if path.is_file() {
        let name = path.strip_prefix(root)?.to_string_lossy().into_owned();
        out.push(CorpusFile {
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
fn tar_gz(files: &[CorpusFile]) -> TestResult<Vec<u8>> {
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

/// Grow the archive's FIRST file by duplicating its leading 600 bytes — a
/// length-changing edit, like a real one-file change. Growing past one 512-byte
/// tar block shifts every downstream tar entry, so every deflate block is cut
/// over different input and the compressed stream is rewritten end to end —
/// an invariant no corpus change can rot, unlike "the largest file" (whose
/// sort position in a real repo decides how much prefix survives). A same-
/// length byte flip is NOT enough: when the perturbed bit stream happens to
/// re-align, CDC resyncs (measured 69% on this corpus, 4% on random bytes).
fn mutate_first(files: &mut [CorpusFile]) -> TestResult<()> {
  let target = files.first_mut().ok_or("no files in corpus")?;
  let dup = target
    .bytes
    .get(..600)
    .ok_or("first file shorter than 600 bytes")?
    .to_vec();
  target.bytes.splice(0..0, dup);
  Ok(())
}

#[tokio::test]
async fn verbatim_chunking_of_a_compressed_archive_barely_dedups() -> TestResult<()> {
  let dir = tempfile::tempdir()?;
  let store = CasStore::new(dir.path().join("cas"), 16384, 1 << 30);

  let mut files = write_corpus(&dir.path().join("corpus"))?;
  let archive_a = tar_gz(&files)?;
  let manifest_a = ingest_bytes(&store, &dir.path().join("a.tgz"), &archive_a).await?;
  let ids_a = chunk_id_set(&manifest_a);

  mutate_first(&mut files)?;
  let archive_b = tar_gz(&files)?;
  let manifest_b = ingest_bytes(&store, &dir.path().join("b.tgz"), &archive_b).await?;
  assert!(
    manifest_b.chunks.len() > 1,
    "corpus must span multiple chunks for the ratio to mean anything"
  );

  let new_bytes = unique_bytes(&manifest_b, &ids_a);
  let total_b = manifest_b.total_size;

  // Record the measured ratio (visible with --nocapture) so the decision is auditable.
  let pct = new_bytes.saturating_mul(100) / total_b.max(1);
  eprintln!(
    "AC-9: second archive {total_b} bytes, {new_bytes} new after a one-file edit ({pct}% unique)"
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
