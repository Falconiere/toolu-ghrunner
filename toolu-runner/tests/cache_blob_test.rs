//! Real-data blob-endpoint tests: a real `CasStore` in a tempdir, the real
//! `blob_router` served over a real TCP socket, driven with `reqwest` issuing
//! the exact Azure op sequences the JS (single-shot) and Go (block) SDKs send.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use reqwest::header::HeaderMap;
use toolu_runner::execution::cache::blob::{BlobRegistry, BlobState, blob_router, sweep_staging};
use toolu_runner::execution::cache::cas::{CasStore, LeaseSet};
use toolu_runner::execution::cache::server::CacheServer;

/// Boxed error alias for test helpers that use `?`.
type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

/// A running blob server plus the handles a test drives it with.
struct Harness {
  _dir: tempfile::TempDir,
  _server: CacheServer,
  base: String,
  registry: BlobRegistry,
  store: CasStore,
  cas_root: PathBuf,
  staging_root: PathBuf,
}

/// Stand up a `BlobState` over a tempdir CAS and serve `blob_router` on a socket.
async fn setup() -> TestResult<Harness> {
  let dir = tempfile::tempdir()?;
  let cas_root = dir.path().join("cas");
  let staging_root = cas_root.join("staging");
  std::fs::create_dir_all(&staging_root)?;
  let registry = BlobRegistry::new();
  let state = Arc::new(BlobState {
    registry: registry.clone(),
    store: CasStore::new(cas_root.clone(), 16384, 1 << 30),
    leases: LeaseSet::new(),
    staging_root: staging_root.clone(),
  });
  let server = CacheServer::start(blob_router(state), "127.0.0.1:0").await?;
  let base = server.base_url().to_owned();
  Ok(Harness {
    _dir: dir,
    _server: server,
    base,
    registry,
    store: CasStore::new(cas_root.clone(), 16384, 1 << 30),
    cas_root,
    staging_root,
  })
}

/// The URL for one blob token under this harness's server.
fn blob_url(base: &str, token: &str) -> String {
  format!("{base}_toolu/blob/{token}")
}

/// A fresh, unique staging file path under `cas/staging`.
fn staging_path(staging_root: &Path) -> PathBuf {
  staging_root.join(uuid::Uuid::new_v4().to_string())
}

/// The real bytes of this repo's workspace `Cargo.lock`, used as the payload.
fn payload() -> TestResult<Vec<u8>> {
  let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../Cargo.lock");
  Ok(std::fs::read(path)?)
}

/// Fail unless all four Azure-required response headers are present.
fn assert_required_headers(headers: &HeaderMap) -> TestResult<()> {
  for name in ["x-ms-request-id", "x-ms-version", "etag", "last-modified"] {
    if !headers.contains_key(name) {
      return Err(format!("missing required header {name}").into());
    }
  }
  Ok(())
}

/// A header's value as a string, or an error if absent or non-ASCII.
fn header_str(headers: &HeaderMap, name: &str) -> TestResult<String> {
  let value = headers
    .get(name)
    .ok_or_else(|| format!("missing header {name}"))?;
  Ok(value.to_str()?.to_owned())
}

/// The first regular file found under `dir`, recursing into subdirs.
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

/// Single-shot Put Blob, asserting 2xx, the required headers, and that the
/// staging file equals the payload. Returns the staging path for ingest.
async fn put_single_shot(
  harness: &Harness,
  client: &reqwest::Client,
  bytes: &[u8],
) -> TestResult<PathBuf> {
  let staging = staging_path(&harness.staging_root);
  let token = harness.registry.mint_upload(
    staging.clone(),
    "refs/heads/main".to_owned(),
    "key".to_owned(),
    "version".to_owned(),
    Duration::from_secs(300),
  );
  let put = client
    .put(blob_url(&harness.base, &token))
    .header("x-ms-blob-type", "BlockBlob")
    .body(bytes.to_vec())
    .send()
    .await?;
  assert!(
    put.status().is_success(),
    "put blob status {}",
    put.status()
  );
  assert_required_headers(put.headers())?;
  assert_eq!(
    std::fs::read(&staging)?.as_slice(),
    bytes,
    "staging != payload"
  );
  Ok(staging)
}

/// HEAD (Content-Length), full GET (200 + exact bytes), and ranged GET
/// (206 + Content-Range + first 100 bytes) against one download URL.
async fn verify_download(client: &reqwest::Client, url: &str, bytes: &[u8]) -> TestResult<()> {
  let head = client.head(url).send().await?;
  assert!(head.status().is_success(), "head status");
  assert_required_headers(head.headers())?;
  assert_eq!(
    header_str(head.headers(), "content-length")?,
    bytes.len().to_string(),
    "HEAD content-length"
  );

  let full = client.get(url).send().await?;
  assert_eq!(full.status().as_u16(), 200, "full get status");
  assert_required_headers(full.headers())?;
  assert_eq!(full.bytes().await?.as_ref(), bytes, "full body");

  let ranged = client.get(url).header("Range", "bytes=0-99").send().await?;
  assert_eq!(ranged.status().as_u16(), 206, "ranged status");
  assert_eq!(
    header_str(ranged.headers(), "content-range")?,
    format!("bytes 0-99/{}", bytes.len()),
    "content-range"
  );
  let want = bytes.get(..100).ok_or("payload shorter than 100 bytes")?;
  assert_eq!(ranged.bytes().await?.as_ref(), want, "ranged body");
  Ok(())
}

#[tokio::test]
async fn single_shot_round_trip() -> TestResult<()> {
  let harness = setup().await?;
  let bytes = payload()?;
  assert!(!bytes.is_empty(), "Cargo.lock payload should be non-empty");
  let client = reqwest::Client::new();

  let staging = put_single_shot(&harness, &client, &bytes).await?;
  let manifest = harness.store.ingest(&staging).await?;
  let download = harness
    .registry
    .mint_download(manifest, Duration::from_secs(300));
  verify_download(&client, &blob_url(&harness.base, &download), &bytes).await?;
  Ok(())
}

#[tokio::test]
async fn block_upload_assembles_in_list_order() -> TestResult<()> {
  let harness = setup().await?;
  let bytes = payload()?;
  let mid = bytes.len() / 2;
  let first = bytes.get(..mid).ok_or("split lower")?.to_vec();
  let second = bytes.get(mid..).ok_or("split upper")?.to_vec();
  let client = reqwest::Client::new();

  let staging = staging_path(&harness.staging_root);
  let token = harness.registry.mint_upload(
    staging.clone(),
    "s".to_owned(),
    "k".to_owned(),
    "v".to_owned(),
    Duration::from_secs(300),
  );
  let url = blob_url(&harness.base, &token);

  let one = client
    .put(format!("{url}?comp=block&blockid=AAAA"))
    .body(first)
    .send()
    .await?;
  assert!(one.status().is_success(), "put block 1 status");
  let two = client
    .put(format!("{url}?comp=block&blockid=AAAB"))
    .body(second)
    .send()
    .await?;
  assert!(two.status().is_success(), "put block 2 status");

  let xml = concat!(
    "<?xml version=\"1.0\" encoding=\"utf-8\"?>",
    "<BlockList><Latest>AAAA</Latest><Latest>AAAB</Latest></BlockList>"
  );
  let commit = client
    .put(format!("{url}?comp=blocklist"))
    .body(xml)
    .send()
    .await?;
  assert!(commit.status().is_success(), "put block list status");
  assert_required_headers(commit.headers())?;
  assert_eq!(
    std::fs::read(&staging)?,
    bytes,
    "assembled != concatenation"
  );
  Ok(())
}

#[tokio::test]
async fn unknown_token_is_forbidden() -> TestResult<()> {
  let harness = setup().await?;
  let client = reqwest::Client::new();
  let url = blob_url(&harness.base, "this-token-was-never-minted");

  let get = client.get(&url).send().await?;
  assert_eq!(get.status().as_u16(), 403, "GET unknown token must be 403");

  let put = client
    .put(&url)
    .header("x-ms-blob-type", "BlockBlob")
    .body(vec![1u8, 2, 3])
    .send()
    .await?;
  assert_eq!(put.status().as_u16(), 403, "PUT unknown token must be 403");
  Ok(())
}

#[tokio::test]
async fn corrupt_chunk_aborts_download() -> TestResult<()> {
  let harness = setup().await?;
  let bytes = payload()?;
  let staging = staging_path(&harness.staging_root);
  std::fs::write(&staging, &bytes)?;
  let manifest = harness.store.ingest(&staging).await?;

  let blobs = harness.cas_root.join("blobs");
  let victim = first_file(&blobs)?.ok_or("no chunk file was written")?;
  let mut chunk = std::fs::read(&victim)?;
  let head = chunk.get_mut(0).ok_or("chunk file was empty")?;
  *head ^= 0xff;
  std::fs::write(&victim, &chunk)?;

  let download = harness
    .registry
    .mint_download(manifest, Duration::from_secs(300));
  let resp = reqwest::Client::new()
    .get(blob_url(&harness.base, &download))
    .send()
    .await?;
  let served_full = matches!(resp.bytes().await, Ok(body) if body.len() == bytes.len());
  assert!(!served_full, "corrupt chunk was served as a complete body");
  Ok(())
}

#[test]
fn sweep_staging_removes_old_entries_only() -> TestResult<()> {
  let dir = tempfile::tempdir()?;
  let staging = dir.path().join("staging");
  std::fs::create_dir_all(&staging)?;
  let old = staging.join("abandoned-upload");
  std::fs::write(&old, b"abandoned")?;
  let fresh = staging.join("in-flight-upload");
  std::fs::write(&fresh, b"in-flight")?;
  filetime::set_file_mtime(&old, filetime::FileTime::from_unix_time(0, 0))?;

  let removed = sweep_staging(&staging, Duration::from_secs(3600))?;
  assert_eq!(removed, 1, "exactly the old entry should be swept");
  assert!(!old.exists(), "old staging entry should be removed");
  assert!(fresh.exists(), "fresh staging entry should remain");
  Ok(())
}
