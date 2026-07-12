//! Regression for the v1 REST cache-poisoning bug. An untrusted job whose write
//! scope is a protected branch (`main`) drives the real
//! reserve -> PATCH -> finalize flow over the v1 router; the write must be
//! refused (403) and the shared `CacheIndex` must stay unpoisoned. A positive
//! control (a trusted job over the same scope) still saves and indexes.
//!
//! Real-data only: a real `CasStore` + `CacheIndex` in a tempdir, served on a
//! real `CacheServer`, driven with `reqwest` and the correct bearer.

use std::path::PathBuf;

use cache::cas::{CacheIndex, CasStore, LeaseSet};
use cache::scope::CacheScopes;
use cache::server::CacheServer;
use cache::trust::TrustLevel;
use cache::v1::{V1Inputs, V1State, v1_router};
use serde_json::{Value, json};

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
