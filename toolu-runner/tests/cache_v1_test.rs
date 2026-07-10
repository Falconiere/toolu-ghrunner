//! Real-HTTP round-trip tests for the legacy v1 REST cache protocol re-hosted
//! on the CAS. No mocks: a real `CasStore` + `CacheIndex` in a tempdir, the real
//! workspace `Cargo.lock` uploaded through reserve → PATCH → finalize and read
//! back byte-for-byte over `reqwest`. Plus a 204 miss, a 401 on a bad/absent
//! bearer, a rejected-unindexed-and-staging-cleaned size mismatch, a 400 on a
//! PATCH without a parseable `Content-Range` (never written at offset 0), a
//! 416 on a malformed/multi `Range`, and AC-10 restart safety (a fresh
//! `CacheIndex` on the same root still finds a finalized entry).

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use toolu_runner::execution::cache::cas::{CacheIndex, CasStore, LeaseSet};
use toolu_runner::execution::cache::scope::CacheScopes;
use toolu_runner::execution::cache::server::CacheServer;
use toolu_runner::execution::cache::trust::TrustLevel;
use toolu_runner::execution::cache::v1::{V1Inputs, V1State, v1_router};

/// Boxed error alias so test helpers can use `?`.
type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

/// The offline bearer token every request presents.
const BEARER: &str = "offline-tok";
/// The single opaque cache version used throughout.
const VERSION: &str = "v-abc";
/// The permissive offline scope backing both write and read.
const SCOPE: &str = "offline";

/// A running v1 cache server plus the handles a test drives it with.
struct Harness {
  _dir: tempfile::TempDir,
  _server: CacheServer,
  base: String,
  cas_root: PathBuf,
}

/// Stand up a `V1State` over a tempdir CAS and serve the v1 router.
async fn setup() -> TestResult<Harness> {
  let dir = tempfile::tempdir()?;
  let cas_root = dir.path().join("cache");
  let staging_root = cas_root.join("staging");
  std::fs::create_dir_all(&staging_root)?;
  let state = V1State::new(V1Inputs {
    store: CasStore::new(cas_root.clone(), 16384, 1 << 30),
    index: CacheIndex::new(cas_root.clone()),
    leases: LeaseSet::new(),
    scopes: CacheScopes {
      write: SCOPE.to_owned(),
      read_ladder: vec![SCOPE.to_owned()],
    },
    trust: TrustLevel::Trusted,
    protected: Vec::new(),
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

/// The real bytes of this repo's workspace `Cargo.lock`, used as the payload.
fn payload() -> TestResult<Vec<u8>> {
  let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../Cargo.lock");
  Ok(std::fs::read(path)?)
}

/// Read a string field from a JSON object, erroring if absent or non-string.
fn str_field(value: &Value, field: &str) -> TestResult<String> {
  value
    .get(field)
    .and_then(Value::as_str)
    .map(str::to_owned)
    .ok_or_else(|| format!("missing string field {field} in {value}").into())
}

/// Absolute URL for a v1 cache API `path` (no leading `/`) under `base`.
///
/// Trims a trailing `/` off `base` and re-adds the separator, so no call site
/// depends on `CacheServer::base_url`'s trailing-slash convention.
fn api_url(base: &str, path: &str) -> String {
  format!("{}/{path}", base.trim_end_matches('/'))
}

/// `POST /caches` to reserve a cache id for `key`.
async fn reserve(client: &reqwest::Client, base: &str, key: &str) -> TestResult<u64> {
  let resp = client
    .post(api_url(base, "_apis/artifactcache/caches"))
    .header("authorization", format!("Bearer {BEARER}"))
    .json(&json!({ "key": key, "version": VERSION }))
    .send()
    .await?;
  assert!(
    resp.status().is_success(),
    "reserve status {}",
    resp.status()
  );
  let body = resp.json::<Value>().await?;
  body
    .get("cacheId")
    .and_then(Value::as_u64)
    .ok_or_else(|| format!("reserve response missing cacheId: {body}").into())
}

/// `PATCH /caches/{id}` writing `data` at `offset` via a `Content-Range` header.
async fn patch_chunk(
  client: &reqwest::Client,
  base: &str,
  cache_id: u64,
  offset: usize,
  data: &[u8],
) -> TestResult<()> {
  let end = offset + data.len() - 1;
  let resp = client
    .patch(api_url(
      base,
      &format!("_apis/artifactcache/caches/{cache_id}"),
    ))
    .header("authorization", format!("Bearer {BEARER}"))
    .header("Content-Range", format!("bytes {offset}-{end}/*"))
    .body(data.to_vec())
    .send()
    .await?;
  assert!(resp.status().is_success(), "patch status {}", resp.status());
  Ok(())
}

/// `POST /caches/{id}` to finalize; returns `(status, body)`.
async fn finalize(
  client: &reqwest::Client,
  base: &str,
  cache_id: u64,
  size: usize,
) -> TestResult<(u16, Value)> {
  let resp = client
    .post(api_url(
      base,
      &format!("_apis/artifactcache/caches/{cache_id}"),
    ))
    .header("authorization", format!("Bearer {BEARER}"))
    .json(&json!({ "size": size }))
    .send()
    .await?;
  let status = resp.status().as_u16();
  Ok((status, resp.json::<Value>().await?))
}

/// `GET /cache?keys=<primary>&version=<VERSION>` with the bearer.
async fn lookup(
  client: &reqwest::Client,
  base: &str,
  primary: &str,
) -> TestResult<reqwest::Response> {
  Ok(
    client
      .get(api_url(
        base,
        &format!("_apis/artifactcache/cache?keys={primary}&version={VERSION}"),
      ))
      .header("authorization", format!("Bearer {BEARER}"))
      .send()
      .await?,
  )
}

/// Reserve → PATCH two chunks → finalize `bytes` under `key`, asserting `ok`.
async fn save_entry(
  client: &reqwest::Client,
  base: &str,
  key: &str,
  bytes: &[u8],
) -> TestResult<()> {
  let cache_id = reserve(client, base, key).await?;
  let mid = bytes.len() / 2;
  let (first, second) = bytes.split_at(mid);
  patch_chunk(client, base, cache_id, 0, first).await?;
  patch_chunk(client, base, cache_id, mid, second).await?;
  let (status, body) = finalize(client, base, cache_id, bytes.len()).await?;
  assert_eq!(status, 200, "finalize status");
  assert_eq!(
    body.get("ok").and_then(Value::as_bool),
    Some(true),
    "finalize ok"
  );
  Ok(())
}

#[tokio::test]
async fn full_round_trip_saves_and_restores() -> TestResult<()> {
  let h = setup().await?;
  let client = reqwest::Client::new();
  let bytes = payload()?;
  assert!(!bytes.is_empty(), "payload should be non-empty");

  save_entry(&client, &h.base, "deps", &bytes).await?;

  let resp = lookup(&client, &h.base, "deps").await?;
  assert_eq!(resp.status().as_u16(), 200, "lookup hit");
  let body = resp.json::<Value>().await?;
  assert_eq!(str_field(&body, "cacheKey")?, "deps", "cacheKey");
  let archive = str_field(&body, "archiveLocation")?;
  assert!(
    archive.contains("/_apis/artifactcache/download/"),
    "archiveLocation shape: {archive}"
  );

  let got = client
    .get(&archive)
    .header("authorization", format!("Bearer {BEARER}"))
    .send()
    .await?
    .bytes()
    .await?;
  assert_eq!(
    got.as_ref(),
    bytes.as_slice(),
    "downloaded bytes != payload"
  );
  Ok(())
}

#[tokio::test]
async fn ranged_download_returns_206_slice() -> TestResult<()> {
  let h = setup().await?;
  let client = reqwest::Client::new();
  let bytes = payload()?;
  save_entry(&client, &h.base, "deps", &bytes).await?;

  let body = lookup(&client, &h.base, "deps")
    .await?
    .json::<Value>()
    .await?;
  let archive = str_field(&body, "archiveLocation")?;

  let resp = client
    .get(&archive)
    .header("authorization", format!("Bearer {BEARER}"))
    .header("Range", "bytes=0-9")
    .send()
    .await?;
  assert_eq!(resp.status().as_u16(), 206, "ranged status");
  let slice = resp.bytes().await?;
  assert_eq!(slice.len(), 10, "ranged length");
  assert_eq!(
    slice.as_ref(),
    bytes.get(0..10).ok_or("payload < 10 bytes")?
  );
  Ok(())
}

#[tokio::test]
async fn download_url_serves_bytes_without_a_bearer() -> TestResult<()> {
  // Real `@actions/cache` / buildx GET the `archiveLocation` with NO
  // Authorization header (a v1 archive URL is a pre-signed capability). The
  // download route must therefore not require a bearer.
  let h = setup().await?;
  let client = reqwest::Client::new();
  let bytes = payload()?;
  save_entry(&client, &h.base, "deps", &bytes).await?;

  let body = lookup(&client, &h.base, "deps")
    .await?
    .json::<Value>()
    .await?;
  let archive = str_field(&body, "archiveLocation")?;

  // No `authorization` header at all — as the real clients send it.
  let resp = client.get(&archive).send().await?;
  assert_eq!(
    resp.status().as_u16(),
    200,
    "an unauthenticated download must succeed"
  );
  let got = resp.bytes().await?;
  assert_eq!(
    got.as_ref(),
    bytes.as_slice(),
    "downloaded bytes != payload"
  );
  Ok(())
}

#[tokio::test]
async fn miss_returns_204() -> TestResult<()> {
  let h = setup().await?;
  let client = reqwest::Client::new();
  let resp = lookup(&client, &h.base, "absent").await?;
  assert_eq!(resp.status().as_u16(), 204, "a miss is 204 No Content");
  Ok(())
}

#[tokio::test]
async fn absent_or_wrong_bearer_is_unauthorized() -> TestResult<()> {
  let h = setup().await?;
  let client = reqwest::Client::new();

  let no_auth = client
    .post(format!("{}_apis/artifactcache/caches", h.base))
    .json(&json!({ "key": "deps", "version": VERSION }))
    .send()
    .await?;
  assert_eq!(no_auth.status().as_u16(), 401, "absent bearer is 401");

  let wrong = client
    .get(format!(
      "{}_apis/artifactcache/cache?keys=deps&version={VERSION}",
      h.base
    ))
    .header("authorization", "Bearer wrong-token")
    .send()
    .await?;
  assert_eq!(wrong.status().as_u16(), 401, "wrong bearer is 401");
  Ok(())
}

#[tokio::test]
async fn size_mismatch_is_rejected_and_not_indexed() -> TestResult<()> {
  let h = setup().await?;
  let client = reqwest::Client::new();
  let bytes = payload()?;

  let cache_id = reserve(&client, &h.base, "deps").await?;
  patch_chunk(&client, &h.base, cache_id, 0, &bytes).await?;
  let (status, body) = finalize(&client, &h.base, cache_id, bytes.len() + 1).await?;
  assert_eq!(status, 400, "size mismatch is 400");
  assert_eq!(
    body.get("ok").and_then(Value::as_bool),
    Some(false),
    "size mismatch ok:false"
  );

  let resp = lookup(&client, &h.base, "deps").await?;
  assert_eq!(resp.status().as_u16(), 204, "a lie must not be indexed");

  let idx = CacheIndex::new(h.cas_root.clone());
  let hit = idx.lookup(&[SCOPE.to_owned()], VERSION, "deps", &[])?;
  assert!(hit.is_none(), "fresh index must not see the rejected entry");

  // The rejected upload's staging file (staging/<cache_id>, created by
  // reserve) must be gone: finalize removes it even when it rejects the size.
  let leftovers: Vec<PathBuf> = std::fs::read_dir(h.cas_root.join("staging"))?
    .map(|e| e.map(|e| e.path()))
    .collect::<Result<_, _>>()?;
  assert!(
    leftovers.is_empty(),
    "rejected finalize must clean up staging, found {leftovers:?}"
  );
  Ok(())
}

/// PATCH `body` to `cache_id` with an optional raw `Content-Range` value;
/// returns the response status.
async fn patch_raw(
  client: &reqwest::Client,
  base: &str,
  cache_id: u64,
  content_range: Option<&str>,
  body: &[u8],
) -> TestResult<u16> {
  let mut req = client
    .patch(api_url(
      base,
      &format!("_apis/artifactcache/caches/{cache_id}"),
    ))
    .header("authorization", format!("Bearer {BEARER}"))
    .body(body.to_vec());
  if let Some(value) = content_range {
    req = req.header("Content-Range", value);
  }
  Ok(req.send().await?.status().as_u16())
}

/// Look up `key` and download its archive; returns the served bytes.
async fn read_back(client: &reqwest::Client, base: &str, key: &str) -> TestResult<Vec<u8>> {
  let body = lookup(client, base, key).await?.json::<Value>().await?;
  let archive = str_field(&body, "archiveLocation")?;
  Ok(client.get(&archive).send().await?.bytes().await?.to_vec())
}

#[tokio::test]
async fn patch_without_parseable_content_range_is_400_and_writes_nothing() -> TestResult<()> {
  // A PATCH whose Content-Range is absent or malformed must be a 400 — never
  // a silent write at offset 0 that corrupts the staged archive.
  let h = setup().await?;
  let client = reqwest::Client::new();
  let bytes = payload()?;
  let cache_id = reserve(&client, &h.base, "deps").await?;
  patch_chunk(&client, &h.base, cache_id, 0, &bytes).await?;

  let absent = patch_raw(&client, &h.base, cache_id, None, b"POISON").await?;
  assert_eq!(absent, 400, "absent Content-Range must be 400");
  for malformed in ["bytes garbage", "bytes -5/x", "0-5/*"] {
    let status = patch_raw(&client, &h.base, cache_id, Some(malformed), b"POISON").await?;
    assert_eq!(status, 400, "Content-Range {malformed:?} must be 400");
  }

  // The rejected PATCHes must not have touched the staging file: finalize and
  // read back the original payload byte-for-byte.
  let (status, _) = finalize(&client, &h.base, cache_id, bytes.len()).await?;
  assert_eq!(status, 200, "finalize after rejected PATCHes");
  let got = read_back(&client, &h.base, "deps").await?;
  assert_eq!(
    got.as_slice(),
    bytes.as_slice(),
    "a rejected PATCH corrupted the staged payload"
  );
  Ok(())
}

#[tokio::test]
async fn malformed_range_on_download_is_416() -> TestResult<()> {
  // A Range we cannot satisfy — wrong unit, multi-range, start > end — must be
  // a 416 with the RFC 9110 `Content-Range: bytes */total` marker, not a 500.
  let h = setup().await?;
  let client = reqwest::Client::new();
  let bytes = payload()?;
  save_entry(&client, &h.base, "deps", &bytes).await?;

  let body = lookup(&client, &h.base, "deps")
    .await?
    .json::<Value>()
    .await?;
  let archive = str_field(&body, "archiveLocation")?;

  for bad in ["items=0-9", "bytes=0-4, 6-9", "bytes=9-2", "bytes=x-9"] {
    let resp = client.get(&archive).header("Range", bad).send().await?;
    assert_eq!(
      resp.status().as_u16(),
      416,
      "Range {bad:?} must be 416, not an internal error"
    );
    let cr = resp
      .headers()
      .get("content-range")
      .and_then(|v| v.to_str().ok())
      .ok_or("416 must carry Content-Range")?;
    assert_eq!(cr, format!("bytes */{}", bytes.len()), "416 Content-Range");
  }
  Ok(())
}

#[tokio::test]
async fn fresh_index_on_same_root_finds_finalized_entry() -> TestResult<()> {
  // AC-10 for v1: the index is restart-safe, so a brand-new `CacheIndex` over
  // the same on-disk root resolves a finalized entry without the server.
  let h = setup().await?;
  let client = reqwest::Client::new();
  let bytes = payload()?;
  save_entry(&client, &h.base, "deps", &bytes).await?;

  let idx = CacheIndex::new(h.cas_root.clone());
  let hit = idx.lookup(&[SCOPE.to_owned()], VERSION, "deps", &[])?;
  let (matched, entry) = hit.ok_or("restart lookup should hit")?;
  assert_eq!(matched, "deps", "matched key");
  assert_eq!(
    entry.size_bytes,
    u64::try_from(bytes.len())?,
    "size persisted"
  );
  Ok(())
}
