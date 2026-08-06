//! Regression for the v1 REST cache-poisoning bug. An untrusted job whose write
//! scope is a protected branch (`main`) drives the real
//! reserve -> PATCH -> finalize flow over the v1 router; the write must be
//! refused (403) and the shared `CacheIndex` must stay unpoisoned. A positive
//! control (a trusted job over the same scope) still saves and indexes.
//!
//! Real-data only: a real `CasStore` + `CacheIndex` in a tempdir, served on a
//! real `CacheServer`, driven with `reqwest` and the correct bearer.

use std::path::{Path, PathBuf};

use cache::cas::{CacheIndex, CasStore, LeaseSet, Manifest};
use cache::scope::CacheScopes;
use cache::server::CacheServer;
use cache::trust::TrustLevel;
use cache::v1::{V1Inputs, V1State, v1_router};
use futures_util::StreamExt;
use serde_json::{Value, json};
use shared::RunnerError;

type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

/// The runtime token the write flow presents (auth is satisfied; trust is not).
const BEARER: &str = "runtime-tok";
/// The single opaque cache version used throughout.
const VERSION: &str = "v-abc";
/// The protected branch scope an untrusted job must not be able to write.
const PROTECTED: &str = "main";

/// A running v1 cache server plus the handles a test drives it with.
struct Harness {
  _dir: tempfile::TempDir,
  _server: CacheServer,
  base: String,
  cas_root: PathBuf,
}

/// Serve a v1 router whose write scope is the protected `main`, at `trust`.
async fn setup(trust: TrustLevel) -> TestResult<Harness> {
  let dir = tempfile::tempdir()?;
  let cas_root = dir.path().join("cache");
  let staging_root = cas_root.join("staging");
  std::fs::create_dir_all(&staging_root)?;
  let state = V1State::new(V1Inputs {
    store: CasStore::new(cas_root.clone(), 16384, 1 << 30),
    index: CacheIndex::new(cas_root.clone()),
    leases: LeaseSet::new(),
    scopes: CacheScopes {
      write: PROTECTED.to_owned(),
      read_ladder: vec![PROTECTED.to_owned()],
    },
    trust,
    protected: vec![PROTECTED.to_owned()],
    bearer: BEARER.to_owned(),
    staging_root,
  });
  let server = CacheServer::start(v1_router(state), "127.0.0.1:0").await?;
  let base = server.base_url().to_owned();
  Ok(Harness {
    _dir: dir,
    _server: server,
    base,
    cas_root,
  })
}

/// `POST /caches` to reserve `key`; returns `(status, cacheId?)`.
async fn reserve(
  client: &reqwest::Client,
  base: &str,
  key: &str,
) -> TestResult<(u16, Option<u64>)> {
  let resp = client
    .post(format!("{base}_apis/artifactcache/caches"))
    .header("authorization", format!("Bearer {BEARER}"))
    .json(&json!({ "key": key, "version": VERSION }))
    .send()
    .await?;
  let status = resp.status().as_u16();
  let id = resp
    .json::<Value>()
    .await
    .ok()
    .and_then(|v| v.get("cacheId").and_then(Value::as_u64));
  Ok((status, id))
}

/// Reserve -> PATCH -> finalize `bytes` under `key`; returns the finalize status.
async fn save(client: &reqwest::Client, base: &str, id: u64, bytes: &[u8]) -> TestResult<u16> {
  let end = bytes.len().saturating_sub(1);
  client
    .patch(format!("{base}_apis/artifactcache/caches/{id}"))
    .header("authorization", format!("Bearer {BEARER}"))
    .header("Content-Range", format!("bytes 0-{end}/*"))
    .body(bytes.to_vec())
    .send()
    .await?;
  let fin = client
    .post(format!("{base}_apis/artifactcache/caches/{id}"))
    .header("authorization", format!("Bearer {BEARER}"))
    .json(&json!({ "size": bytes.len() }))
    .send()
    .await?;
  Ok(fin.status().as_u16())
}

#[tokio::test]
async fn untrusted_v1_write_to_protected_scope_is_refused() -> TestResult<()> {
  let h = setup(TrustLevel::Untrusted).await?;
  let client = reqwest::Client::new();

  let (status, id) = reserve(&client, &h.base, "poison").await?;
  assert_eq!(
    status, 403,
    "untrusted reserve on a protected scope must be 403"
  );
  assert!(
    id.is_none(),
    "a refused reserve must not allocate a cache id"
  );

  // Nothing was indexed: a fresh index over the same root sees no poison.
  let idx = CacheIndex::new(h.cas_root.clone());
  let hit = idx.lookup(&[PROTECTED.to_owned()], VERSION, "poison", &[])?;
  assert!(hit.is_none(), "the protected scope must not be poisoned");
  Ok(())
}

#[tokio::test]
async fn trusted_v1_write_to_protected_scope_succeeds() -> TestResult<()> {
  let h = setup(TrustLevel::Trusted).await?;
  let client = reqwest::Client::new();
  let bytes = b"trusted cache payload for the protected scope";

  let (status, id) = reserve(&client, &h.base, "deps").await?;
  assert_eq!(status, 200, "trusted reserve must succeed");
  let id = id.ok_or("trusted reserve must return a cacheId")?;
  let fin = save(&client, &h.base, id, bytes).await?;
  assert_eq!(fin, 200, "trusted finalize must succeed");

  let idx = CacheIndex::new(h.cas_root.clone());
  let hit = idx.lookup(&[PROTECTED.to_owned()], VERSION, "deps", &[])?;
  assert!(
    hit.is_some(),
    "a trusted write must land in the protected scope"
  );
  Ok(())
}

// --- T5b: CAS self-heal on a BLAKE3 digest mismatch --------------------

/// Content-addressed blob path: `<root>/<sub>/<hex[0..2]>/<hex>` — mirrors the
/// CAS layer's private `chunk_io::blob_path` layout convention, which is
/// itself stable on-disk (chunk/manifest paths are the store's addressing
/// scheme, not an implementation detail that can change silently).
fn blob_path(root: &Path, sub: &str, hex: &str) -> PathBuf {
  let shard = hex.get(0..2).unwrap_or("00");
  root.join(sub).join(shard).join(hex)
}

/// Collect `store.read_range` for the whole manifest, stopping at the first
/// error. Returns the bytes successfully yielded before that point (empty if
/// the very first chunk failed) and whether an error was seen at all.
async fn collect_until_error(store: &CasStore, m: &Manifest) -> (Vec<u8>, bool) {
  let stream = store.read_range(m, 0, m.total_size);
  futures_util::pin_mut!(stream);
  let mut out = Vec::new();
  let mut saw_err = false;
  while let Some(item) = stream.next().await {
    if let Ok(bytes) = item {
      out.extend_from_slice(&bytes);
    } else {
      saw_err = true;
      break;
    }
  }
  (out, saw_err)
}

/// A real multi-chunk ingest with its second chunk already torn. `_dir` must
/// be held for the fixture's lifetime — it owns the on-disk root `victim_path`
/// points into.
struct CorruptChunkFixture {
  _dir: tempfile::TempDir,
  store: CasStore,
  manifest: Manifest,
  original: Vec<u8>,
  victim_path: PathBuf,
}

/// Ingest a real payload (this repo's `Cargo.lock`, 16 KiB avg chunks) and
/// truncate its SECOND chunk (not the first), so a later read legitimately
/// yields chunk 0's bytes before hitting the bad one.
async fn seed_corrupt_second_chunk() -> TestResult<CorruptChunkFixture> {
  let dir = tempfile::tempdir()?;
  let root = dir.path().join("cache");
  let store = CasStore::new(root.clone(), 16384, 1 << 30);
  let payload = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock");
  let original = std::fs::read(&payload)?;
  let manifest = store.ingest(&payload).await?;
  let victim = manifest
    .chunks
    .get(1)
    .ok_or("fixture must span multiple chunks so a legitimate prefix precedes the corrupt one")?;
  let victim_path = blob_path(&root, "blobs", &victim.id.to_hex());
  assert!(
    victim_path.exists(),
    "expected the second chunk's file to exist before corruption"
  );
  std::fs::write(&victim_path, b"torn")?;
  Ok(CorruptChunkFixture {
    _dir: dir,
    store,
    manifest,
    original,
    victim_path,
  })
}

/// AC-8 (first half): the first read over a torn chunk is a documented,
/// pre-existing **aborted body** — it still returns every good byte before
/// the corrupt chunk, then errors, rather than failing cleanly up front — but
/// self-heal removes the bad file as a side effect. Returns the prefix length.
async fn assert_first_read_is_aborted_then_heals(fx: &CorruptChunkFixture) -> TestResult<usize> {
  let (bytes, saw_err) = collect_until_error(&fx.store, &fx.manifest).await;
  assert!(saw_err, "reading a corrupt chunk must surface an error");
  assert!(
    !bytes.is_empty(),
    "the first (good) chunk's bytes must still come through before the abort"
  );
  assert!(
    (bytes.len() as u64) < fx.manifest.total_size,
    "the read must abort before the full body, not silently complete"
  );
  let expected_prefix = fx
    .original
    .get(..bytes.len())
    .ok_or("aborted prefix must not exceed the original payload length")?;
  assert_eq!(
    bytes, expected_prefix,
    "bytes yielded before the abort must be correct, unmangled data"
  );
  assert!(
    !fx.victim_path.exists(),
    "self-heal must remove the corrupt chunk file after the first read"
  );
  Ok(bytes.len())
}

/// AC-8 (second half): with the chunk now simply absent, the second read is a
/// clean miss — same error boundary as the first, no truncated bytes read
/// again, and the file never reappears.
async fn assert_second_read_is_clean_miss(fx: &CorruptChunkFixture, first_prefix_len: usize) {
  let (bytes, saw_err) = collect_until_error(&fx.store, &fx.manifest).await;
  assert!(
    saw_err,
    "the still-missing chunk must miss again on the second lookup"
  );
  assert_eq!(
    bytes.len(),
    first_prefix_len,
    "the clean-miss prefix must match the first read's — nothing torn reappears"
  );
  assert!(
    !fx.victim_path.exists(),
    "the corrupt chunk must not reappear after a clean miss"
  );
}

/// AC-8: truncating a chunk file self-heals — the first read is a documented
/// aborted body (asserted as such, not a clean miss), and the second lookup
/// is a clean miss.
#[tokio::test]
async fn corrupt_chunk_self_heals_on_second_lookup() -> TestResult<()> {
  let fx = seed_corrupt_second_chunk().await?;
  let first_prefix_len = assert_first_read_is_aborted_then_heals(&fx).await?;
  assert_second_read_is_clean_miss(&fx, first_prefix_len).await;
  Ok(())
}

/// AC-8b: a manifest is NEVER removed by the self-heal path, even when its
/// own content fails BLAKE3 verification.
#[tokio::test]
async fn corrupt_manifest_is_never_removed() -> TestResult<()> {
  let dir = tempfile::tempdir()?;
  let root = dir.path().join("cache");
  let store = CasStore::new(root.clone(), 16384, 1 << 30);

  let payload = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock");
  let m = store.ingest(&payload).await?;
  let manifest_id = store.put_manifest(&m).await?;
  let hex = manifest_id.to_hex();
  let manifest_path = blob_path(&root, "manifests", &hex);
  assert!(
    manifest_path.exists(),
    "expected the manifest file to exist after put_manifest"
  );
  std::fs::write(&manifest_path, b"corrupt-manifest-bytes")?;

  let result = store.get_manifest(&manifest_id).await;
  assert!(
    result.is_err(),
    "a corrupt manifest must fail BLAKE3 verification"
  );
  assert!(
    manifest_path.exists(),
    "a corrupt MANIFEST must never be removed by the self-heal path — that path only ever \
     touches chunk files"
  );
  Ok(())
}

// --- AC-8c: WARN-and-continue when the corrupt chunk's own removal fails --

/// Like `collect_until_error`, but returns the actual error (if any) instead
/// of just a boolean, so a caller can assert on ITS SHAPE — distinguishing an
/// ordinary miss from some other, unhandled failure (e.g. a removal error
/// leaking out of the self-heal path instead of being warned-and-ignored).
async fn collect_until_error_with_err(
  store: &CasStore,
  m: &Manifest,
) -> (Vec<u8>, Option<RunnerError>) {
  let stream = store.read_range(m, 0, m.total_size);
  futures_util::pin_mut!(stream);
  let mut out = Vec::new();
  let mut err = None;
  while let Some(item) = stream.next().await {
    match item {
      Ok(bytes) => out.extend_from_slice(&bytes),
      Err(e) => {
        err = Some(e);
        break;
      },
    }
  }
  (out, err)
}

/// Best-effort: make `dir` immutable via Linux `chattr +i`, so files inside
/// it cannot be created or removed by anyone — root included — unless the
/// process holds `CAP_LINUX_IMMUTABLE` (commonly dropped inside a
/// container's default capability set). `chmod`-based read-only is not used
/// here because root bypasses directory write permission checks outright
/// (verified by hand: `chmod 555` on a directory does not stop `root` from
/// unlinking a file inside it).
///
/// Returns whether the flag is actually ENFORCED, not merely accepted, by
/// round-tripping a throwaway file — so an environment where `chattr`
/// silently has no effect (unsupported filesystem, or a process that does
/// hold `CAP_LINUX_IMMUTABLE`) is detected rather than assumed.
fn try_make_dir_removal_proof(dir: &Path) -> bool {
  let Ok(status) = std::process::Command::new("chattr")
    .arg("+i")
    .arg(dir)
    .status()
  else {
    return false; // `chattr` not available on this host.
  };
  if !status.success() {
    return false;
  }
  let probe = dir.join(".removal_proof_probe");
  if std::fs::write(&probe, b"x").is_ok() {
    // Creation inside the "immutable" dir was NOT blocked either — the flag
    // had no effect here; undo it and report unenforced.
    let _ = std::fs::remove_file(&probe);
    clear_dir_removal_proof(dir);
    return false;
  }
  true
}

/// Undo `try_make_dir_removal_proof`. Best-effort test cleanup only.
fn clear_dir_removal_proof(dir: &Path) {
  let _ = std::process::Command::new("chattr")
    .arg("-i")
    .arg(dir)
    .status();
}

/// RAII guard so the immutable flag is cleared even if an assertion below
/// panics (`tempfile::TempDir`'s own `Drop` would otherwise try to recurse
/// into a directory it can no longer delete from).
struct RemovalProofGuard<'a>(&'a Path);

impl Drop for RemovalProofGuard<'_> {
  fn drop(&mut self) {
    clear_dir_removal_proof(self.0);
  }
}

/// AC-8c: when the corrupt chunk's own removal fails (its shard directory is
/// immutable), the self-heal path's WARN-and-continue arm must still fall
/// through to the SAME miss a successful removal would produce — not
/// propagate the removal's own I/O error, and not panic.
#[tokio::test]
async fn corrupt_chunk_removal_failure_still_falls_through_to_miss() -> TestResult<()> {
  let fx = seed_corrupt_second_chunk().await?;
  let shard_dir = fx
    .victim_path
    .parent()
    .ok_or("victim chunk path has no parent directory")?;

  if !try_make_dir_removal_proof(shard_dir) {
    eprintln!(
      "skipping corrupt_chunk_removal_failure_still_falls_through_to_miss: this \
       environment does not enforce directory immutability against the current \
       process (root commonly holds CAP_LINUX_IMMUTABLE outside a container, or \
       the filesystem lacks chattr support), so the removal-failure arm cannot \
       be exercised here"
    );
    return Ok(());
  }
  let _guard = RemovalProofGuard(shard_dir);

  let (bytes, err) = collect_until_error_with_err(&fx.store, &fx.manifest).await;
  let err =
    err.ok_or("reading a chunk whose corrupt-removal is blocked must still surface an error")?;
  assert!(
    !bytes.is_empty(),
    "the first (good) chunk's bytes must still come through before the abort"
  );
  assert!(
    matches!(err, RunnerError::Cache(_)),
    "a blocked removal must still surface the ordinary miss error kind (Cache), \
     not an unhandled I/O error from the failed remove_file, got {err:?}"
  );
  assert!(
    err.to_string().contains("missing chunk"),
    "expected the ordinary miss message, got: {err}"
  );
  assert!(
    fx.victim_path.exists(),
    "the corrupt chunk file must still exist — its removal was blocked by the \
     immutable shard directory, and the WARN-and-continue arm must not have \
     removed it some other way"
  );
  Ok(())
}
