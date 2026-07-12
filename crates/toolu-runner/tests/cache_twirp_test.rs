//! Real-HTTP tests for the Cache Service v2 Twirp RPCs, driven end to end over
//! the merged `cache_router` (Twirp + blob) on a real `CacheServer` socket with
//! `reqwest`. No mocks: a real `CasStore` in a tempdir, a real `Cargo.lock`
//! payload uploaded through the blob endpoint and read back byte-for-byte.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};
use cache::blob::BlobRegistry;
use cache::cas::{CacheIndex, CasStore, LeaseSet};
use cache::scope::CacheScopes;
use cache::server::CacheServer;
use cache::trust::TrustLevel;
use cache::twirp::{TwirpState, cache_router};

/// Boxed error alias so test helpers can use `?`.
type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

/// One test's scope / trust / auth configuration.
struct Setup {
  write: String,
  ladder: Vec<String>,
  trust: TrustLevel,
  protected: Vec<String>,
  bearer: String,
}

impl Setup {
  /// A trusted job on `refs/heads/main`, no protected scopes, bearer `tok`.
  fn trusted_main() -> Self {
    Self {
      write: "refs/heads/main".to_owned(),
      ladder: vec!["refs/heads/main".to_owned()],
      trust: TrustLevel::Trusted,
      protected: Vec::new(),
      bearer: "tok".to_owned(),
    }
  }
}

/// A running cache server plus the handles a test drives it with.
struct Harness {
  _dir: tempfile::TempDir,
  _server: CacheServer,
  base: String,
  bearer: String,
  cas_root: PathBuf,
}

/// Stand up a `TwirpState` over a tempdir CAS and serve the merged router.
async fn setup(cfg: Setup) -> TestResult<Harness> {
  let dir = tempfile::tempdir()?;
  let cas_root = dir.path().join("cas");
  let staging_root = cas_root.join("staging");
  std::fs::create_dir_all(&staging_root)?;
  let state = TwirpState {
    store: CasStore::new(cas_root.clone(), 16384, 1 << 30),
    index: CacheIndex::new(cas_root.clone()),
    registry: BlobRegistry::new(),
    leases: LeaseSet::new(),
    scopes: CacheScopes {
      write: cfg.write,
      read_ladder: cfg.ladder,
    },
    trust: cfg.trust,
    protected: cfg.protected,
    bearer: cfg.bearer.clone(),
    staging_root,
    upload_ttl: Duration::from_secs(300),
    download_ttl: Duration::from_secs(300),
  };
  let server = CacheServer::start(cache_router(state), "127.0.0.1:0").await?;
  let base = server.base_url().to_owned();
  Ok(Harness {
    _dir: dir,
    _server: server,
    base,
    bearer: cfg.bearer,
    cas_root,
  })
}

/// The full URL for one `CacheService` method under this harness's server.
fn rpc_url(base: &str, method: &str) -> String {
  format!("{base}twirp/github.actions.results.api.v1.CacheService/{method}")
}

/// POST `body` to `method` with this harness's bearer token.
async fn post_rpc(
  h: &Harness,
  client: &reqwest::Client,
  method: &str,
  body: Value,
) -> TestResult<reqwest::Response> {
  Ok(
    client
      .post(rpc_url(&h.base, method))
      .header("authorization", format!("Bearer {}", h.bearer))
      .json(&body)
      .send()
      .await?,
  )
}

/// The real bytes of this repo's workspace `Cargo.lock`, used as the payload.
fn payload() -> TestResult<Vec<u8>> {
  let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock");
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

/// Create + PUT + Finalize one entry, returning the parsed Finalize response.
async fn save_entry(
  h: &Harness,
  client: &reqwest::Client,
  key: &str,
  version: &str,
  bytes: &[u8],
) -> TestResult<Value> {
  let create = post_rpc(
    h,
    client,
    "CreateCacheEntry",
    json!({"key": key, "version": version}),
  )
  .await?
  .json::<Value>()
  .await?;
  let upload_url = str_field(&create, "signed_upload_url")?;
  let put = client
    .put(&upload_url)
    .header("x-ms-blob-type", "BlockBlob")
    .body(bytes.to_vec())
    .send()
    .await?;
  assert!(
    put.status().is_success(),
    "put blob status {}",
    put.status()
  );
  let finalize = post_rpc(
    h,
    client,
    "FinalizeCacheEntryUpload",
    json!({"key": key, "size_bytes": bytes.len().to_string(), "version": version}),
  )
  .await?
  .json::<Value>()
  .await?;
  Ok(finalize)
}

#[tokio::test]
async fn full_round_trip_saves_and_restores() -> TestResult<()> {
  let h = setup(Setup::trusted_main()).await?;
  let client = reqwest::Client::new();
  let bytes = payload()?;
  assert!(!bytes.is_empty(), "payload should be non-empty");

  let finalize = save_entry(&h, &client, "deps", "v1", &bytes).await?;
  assert_eq!(
    finalize.get("ok").and_then(Value::as_bool),
    Some(true),
    "finalize ok"
  );
  let entry_id = str_field(&finalize, "entry_id")?;
  assert!(
    entry_id.chars().all(|c| c.is_ascii_digit()) && !entry_id.is_empty(),
    "entry_id must be a decimal string, got {entry_id:?}"
  );

  let download = post_rpc(
    &h,
    &client,
    "GetCacheEntryDownloadURL",
    json!({"key": "deps", "restore_keys": [], "version": "v1"}),
  )
  .await?
  .json::<Value>()
  .await?;
  assert_eq!(
    download.get("ok").and_then(Value::as_bool),
    Some(true),
    "download ok"
  );
  assert_eq!(str_field(&download, "matched_key")?, "deps", "matched_key");

  let url = str_field(&download, "signed_download_url")?;
  let got = client.get(&url).send().await?.bytes().await?;
  assert_eq!(
    got.as_ref(),
    bytes.as_slice(),
    "downloaded bytes != payload"
  );
  Ok(())
}

#[tokio::test]
async fn miss_returns_bare_ok_false() -> TestResult<()> {
  let h = setup(Setup::trusted_main()).await?;
  let client = reqwest::Client::new();

  let resp = post_rpc(
    &h,
    &client,
    "GetCacheEntryDownloadURL",
    json!({"key": "absent", "restore_keys": [], "version": "v1"}),
  )
  .await?;
  assert_eq!(
    resp.status().as_u16(),
    200,
    "a miss is HTTP 200, not a Twirp error"
  );
  // Object equality, not a string compare: order- and whitespace-independent,
  // and still fails if the handler grows an extra field (e.g. a null URL).
  let body = resp.json::<Value>().await?;
  assert_eq!(
    body,
    json!({"ok": false}),
    "miss body must carry ok:false and nothing else"
  );
  Ok(())
}

#[tokio::test]
async fn restore_key_prefix_and_version_isolation() -> TestResult<()> {
  let h = setup(Setup::trusted_main()).await?;
  let client = reqwest::Client::new();
  let bytes = payload()?;
  save_entry(&h, &client, "deps-abc", "v1", &bytes).await?;

  let hit = post_rpc(
    &h,
    &client,
    "GetCacheEntryDownloadURL",
    json!({"key": "deps-zzz", "restore_keys": ["deps-"], "version": "v1"}),
  )
  .await?
  .json::<Value>()
  .await?;
  assert_eq!(
    hit.get("ok").and_then(Value::as_bool),
    Some(true),
    "restore-key hit"
  );
  assert_eq!(
    str_field(&hit, "matched_key")?,
    "deps-abc",
    "matched newest prefix"
  );

  let wrong_version = post_rpc(
    &h,
    &client,
    "GetCacheEntryDownloadURL",
    json!({"key": "deps-abc", "restore_keys": [], "version": "v2"}),
  )
  .await?
  .json::<Value>()
  .await?;
  assert_eq!(
    wrong_version.get("ok").and_then(Value::as_bool),
    Some(false),
    "a different version must miss"
  );
  Ok(())
}

#[tokio::test]
async fn write_to_protected_scope_is_denied() -> TestResult<()> {
  let h = setup(Setup {
    write: "refs/heads/main".to_owned(),
    ladder: vec!["refs/heads/main".to_owned()],
    trust: TrustLevel::Untrusted,
    protected: vec!["refs/heads/main".to_owned()],
    bearer: "tok".to_owned(),
  })
  .await?;
  let client = reqwest::Client::new();

  let resp = post_rpc(
    &h,
    &client,
    "CreateCacheEntry",
    json!({"key": "deps", "version": "v1"}),
  )
  .await?
  .json::<Value>()
  .await?;
  assert_eq!(
    resp.get("ok").and_then(Value::as_bool),
    Some(false),
    "protected write denied"
  );
  let message = str_field(&resp, "message")?;
  assert!(
    message.starts_with("cache write denied:"),
    "message must carry the load-bearing prefix, got {message:?}"
  );
  Ok(())
}

#[tokio::test]
async fn duplicate_entry_is_reported() -> TestResult<()> {
  let h = setup(Setup::trusted_main()).await?;
  let client = reqwest::Client::new();
  let bytes = payload()?;
  save_entry(&h, &client, "deps", "v1", &bytes).await?;

  let resp = post_rpc(
    &h,
    &client,
    "CreateCacheEntry",
    json!({"key": "deps", "version": "v1"}),
  )
  .await?
  .json::<Value>()
  .await?;
  assert_eq!(
    resp.get("ok").and_then(Value::as_bool),
    Some(false),
    "duplicate refused"
  );
  assert_eq!(
    str_field(&resp, "message")?,
    "cache entry already exists",
    "duplicate message"
  );
  Ok(())
}

#[tokio::test]
async fn missing_chunk_yields_ok_false_not_500() -> TestResult<()> {
  let h = setup(Setup::trusted_main()).await?;
  let client = reqwest::Client::new();
  let bytes = payload()?;
  save_entry(&h, &client, "deps", "v1", &bytes).await?;

  // Delete every chunk blob out from under the live manifest.
  std::fs::remove_dir_all(h.cas_root.join("blobs"))?;

  let resp = post_rpc(
    &h,
    &client,
    "GetCacheEntryDownloadURL",
    json!({"key": "deps", "restore_keys": [], "version": "v1"}),
  )
  .await?;
  assert_eq!(
    resp.status().as_u16(),
    200,
    "a missing chunk must be 200, never 500"
  );
  let body = resp.json::<Value>().await?;
  assert_eq!(
    body.get("ok").and_then(Value::as_bool),
    Some(false),
    "a missing chunk resolves to a miss"
  );
  Ok(())
}

#[tokio::test]
async fn request_without_bearer_is_unauthorized() -> TestResult<()> {
  let h = setup(Setup::trusted_main()).await?;
  let client = reqwest::Client::new();

  let resp = client
    .post(rpc_url(&h.base, "CreateCacheEntry"))
    .json(&json!({"key": "deps", "version": "v1"}))
    .send()
    .await?;
  assert_eq!(resp.status().as_u16(), 401, "missing bearer must be 401");
  Ok(())
}

#[tokio::test]
async fn snake_case_string_ints_and_ignored_metadata() -> TestResult<()> {
  let h = setup(Setup::trusted_main()).await?;
  let client = reqwest::Client::new();
  let bytes = payload()?;

  // CreateCacheEntry must accept and ignore an extra `metadata` field.
  let create = post_rpc(
    &h,
    &client,
    "CreateCacheEntry",
    json!({"key": "deps", "version": "v1", "metadata": {}}),
  )
  .await?
  .json::<Value>()
  .await?;
  let upload_url = str_field(&create, "signed_upload_url")?;
  let put = client
    .put(&upload_url)
    .header("x-ms-blob-type", "BlockBlob")
    .body(bytes.clone())
    .send()
    .await?;
  assert!(
    put.status().is_success(),
    "put blob status {}",
    put.status()
  );

  // Finalize's entry_id must be a JSON *string* of digits, not a number.
  let text = post_rpc(
    &h,
    &client,
    "FinalizeCacheEntryUpload",
    json!({"key": "deps", "size_bytes": bytes.len().to_string(), "version": "v1"}),
  )
  .await?
  .text()
  .await?;
  assert!(
    text.contains(r#""entry_id":""#),
    "entry_id must serialize as a string, got {text}"
  );
  let parsed = serde_json::from_str::<Value>(&text)?;
  let entry_id = str_field(&parsed, "entry_id")?;
  assert!(
    entry_id.chars().all(|c| c.is_ascii_digit()) && !entry_id.is_empty(),
    "entry_id must be decimal digits, got {entry_id:?}"
  );
  Ok(())
}
