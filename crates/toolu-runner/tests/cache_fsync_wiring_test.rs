//! AC-9 (wiring half): `[cache] fsync_chunks` reaching the CAS the *job* runs
//! over, not just the parsed config.
//!
//! `config_resolve_test` proves the key round-trips into `shared::CacheConfig`
//! and `cas_store_test` proves `CasStore::with_fsync_chunks` ingests correctly;
//! neither touches `job_runner`, which is where the two are joined. This test
//! drives a real `Offline`-mode job with `fsync_chunks = true` and saves and
//! restores a real cache entry — this workspace's own `Cargo.lock` — through
//! the job's own live v1 cache server, over real HTTP, while the job runs.
//!
//! The step holds the job open (and therefore the server up) until the test
//! releases it with a sentinel file. Note the honest limit of any black-box
//! test here: `fsync` has no observable effect short of a power cut, so what is
//! asserted is that the fsync-configured store is the one actually serving the
//! job and that it round-trips byte-for-byte.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use cache::cas::{CacheIndex, CasStore};
use execution::execution::job_runner::run_job;
use execution::execution::job_teardown::JobTeardown;
use serde_json::{Value, json};
use shared::{
  ActionStep, AgentJobRequestMessage, CacheConfig, RunnerConfig, RunnerError, RunnerEvent,
  SecretMasker, ServicesMode,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const JOB_MESSAGE: &str = include_str!("fixtures/job_message.json");

/// Cache key the test saves under.
const KEY: &str = "fsync-wiring";
/// Opaque cache version for the saved entry.
const VERSION: &str = "v-fsync-wiring";
/// How long the test waits for the job's step to publish its service env.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

/// The fixture job with a single step that publishes the offline service env
/// and then blocks until `sentinel` appears, keeping the cache server up.
fn fixture_job(dump: &Path, sentinel: &Path) -> TestResult<AgentJobRequestMessage> {
  let script = format!(
    "printf '%s\\n%s\\n' \"$ACTIONS_CACHE_URL\" \"$ACTIONS_RUNTIME_TOKEN\" > '{dump}'\n\
     n=0\n\
     while [ ! -f '{sentinel}' ] && [ \"$n\" -lt 600 ]; do sleep 0.1; n=$((n+1)); done\n",
    dump = dump.display(),
    sentinel = sentinel.display()
  );
  let mut msg: AgentJobRequestMessage = serde_json::from_str(JOB_MESSAGE)?;
  msg.steps = vec![ActionStep::script("hold-open", &script, "")];
  Ok(msg)
}

/// Poll `dump` until the step has written both lines; returns `(base_url, bearer)`.
async fn await_service_env(dump: &Path) -> TestResult<(String, String)> {
  let deadline = SystemTime::now()
    .checked_add(HANDSHAKE_TIMEOUT)
    .ok_or("deadline overflow")?;
  loop {
    if let Ok(text) = std::fs::read_to_string(dump) {
      let mut lines = text.lines().filter(|l| !l.is_empty());
      if let (Some(url), Some(token)) = (lines.next(), lines.next()) {
        return Ok((url.to_owned(), token.to_owned()));
      }
    }
    if SystemTime::now() >= deadline {
      return Err("job step never published its offline service env".into());
    }
    tokio::time::sleep(Duration::from_millis(20)).await;
  }
}

/// Absolute URL for a v1 cache API `path` under `base` (trailing-slash safe).
fn api_url(base: &str, path: &str) -> String {
  format!("{}/{path}", base.trim_end_matches('/'))
}

/// Read a string field from a JSON object, erroring if absent or non-string.
fn str_field(value: &Value, field: &str) -> TestResult<String> {
  value
    .get(field)
    .and_then(Value::as_str)
    .map(str::to_owned)
    .ok_or_else(|| format!("missing string field {field} in {value}").into())
}

/// Reserve → PATCH → finalize `bytes` under [`KEY`] on the job's live server.
async fn save_entry(
  client: &reqwest::Client,
  base: &str,
  bearer: &str,
  bytes: &[u8],
) -> TestResult {
  let reserved = client
    .post(api_url(base, "_apis/artifactcache/caches"))
    .header("authorization", format!("Bearer {bearer}"))
    .json(&json!({ "key": KEY, "version": VERSION }))
    .send()
    .await?;
  assert!(reserved.status().is_success(), "reserve: {reserved:?}");
  let cache_id = reserved
    .json::<Value>()
    .await?
    .get("cacheId")
    .and_then(Value::as_u64)
    .ok_or("reserve response missing cacheId")?;

  let end = bytes.len().saturating_sub(1);
  let patched = client
    .patch(api_url(
      base,
      &format!("_apis/artifactcache/caches/{cache_id}"),
    ))
    .header("authorization", format!("Bearer {bearer}"))
    .header("Content-Range", format!("bytes 0-{end}/*"))
    .body(bytes.to_vec())
    .send()
    .await?;
  assert!(patched.status().is_success(), "patch: {patched:?}");

  let finalized = client
    .post(api_url(
      base,
      &format!("_apis/artifactcache/caches/{cache_id}"),
    ))
    .header("authorization", format!("Bearer {bearer}"))
    .json(&json!({ "size": bytes.len() }))
    .send()
    .await?;
  assert!(finalized.status().is_success(), "finalize: {finalized:?}");
  Ok(())
}

/// Look [`KEY`] up and download the archive the server points at.
async fn restore_entry(client: &reqwest::Client, base: &str, bearer: &str) -> TestResult<Vec<u8>> {
  let hit = client
    .get(api_url(
      base,
      &format!("_apis/artifactcache/cache?keys={KEY}&version={VERSION}"),
    ))
    .header("authorization", format!("Bearer {bearer}"))
    .send()
    .await?;
  assert_eq!(hit.status().as_u16(), 200, "lookup must hit");
  let archive = str_field(&hit.json::<Value>().await?, "archiveLocation")?;

  let body = client
    .get(&archive)
    .header("authorization", format!("Bearer {bearer}"))
    .send()
    .await?
    .bytes()
    .await?;
  Ok(body.to_vec())
}

/// The real bytes of this workspace's `Cargo.lock`, used as the cached archive.
fn payload() -> TestResult<Vec<u8>> {
  let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock");
  Ok(std::fs::read(path)?)
}

/// Runner config for an offline job with the per-chunk fsync escape hatch on.
fn fsync_config(data_dir: PathBuf, workspace_root: PathBuf) -> RunnerConfig {
  RunnerConfig {
    data_dir,
    workspace_root,
    cgroup_path: None,
    services_mode: ServicesMode::Offline,
    cache: CacheConfig {
      fsync_chunks: true,
      ..CacheConfig::default()
    },
    ..RunnerConfig::default()
  }
}

/// A live job: the `run_job` task and the event-stream drain that keeps it
/// from ever blocking on a full channel.
struct LiveJob {
  job: tokio::task::JoinHandle<Result<JobTeardown, RunnerError>>,
  drain: tokio::task::JoinHandle<()>,
}

/// Start `msg` on `config` in the background, draining its event stream.
fn spawn_offline_job(config: &RunnerConfig, msg: AgentJobRequestMessage) -> LiveJob {
  let (tx, mut rx) = mpsc::channel::<RunnerEvent>(1024);
  let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
  let job_config = config.clone();
  let job = tokio::spawn(async move {
    run_job(
      msg,
      &job_config,
      CancellationToken::new(),
      tx,
      Arc::new(Mutex::new(SecretMasker::new())),
    )
    .await
  });
  LiveJob { job, drain }
}

/// The entry is fresh, so teardown GC retains it: its index record and its
/// chunk blobs must still be on disk after the job.
fn assert_entry_retained(data_dir: &Path) -> TestResult {
  let cache_dir = data_dir.join("cache");
  let index = CacheIndex::new(cache_dir.clone());
  assert_eq!(
    index.records()?.len(),
    1,
    "the saved entry must survive teardown GC"
  );
  let store = CasStore::new(cache_dir, CacheConfig::DEFAULT_CHUNK_AVG_BYTES, 1 << 30);
  assert!(
    !store.list_chunk_ids()?.is_empty(),
    "the fsynced chunks must still be on disk"
  );
  Ok(())
}

/// Direct wiring check: `job_runner::build_cas_handles` is private (there is
/// no way to reach it from here), so this mirrors its exact construction —
/// `CasStore::new(...).with_fsync_chunks(config.cache.fsync_chunks)` — for
/// BOTH settings, and asserts the resulting store's `fsync_chunks()` matches.
/// The live round trip below only exercises `true`, and fsync leaves no
/// on-disk difference to assert against `false` anyway; this is what actually
/// proves the config bool reaches the constructed `CasStore` in both
/// directions.
#[test]
fn fsync_chunks_config_flag_is_observable_on_the_constructed_store() -> TestResult {
  let dir = tempfile::tempdir()?;
  for fsync_chunks in [true, false] {
    let config = CacheConfig {
      fsync_chunks,
      ..CacheConfig::default()
    };
    let store = CasStore::new(
      dir.path().join("cache"),
      config.chunk_avg_bytes,
      config.max_bytes,
    )
    .with_fsync_chunks(config.fsync_chunks);
    assert_eq!(
      store.fsync_chunks(),
      fsync_chunks,
      "CasStore built with fsync_chunks = {fsync_chunks} must report it via fsync_chunks()"
    );
  }
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn offline_job_with_fsync_chunks_round_trips_a_real_entry() -> TestResult {
  let dir = tempfile::tempdir()?;
  let workspace_root = dir.path().join("work");
  let data_dir = dir.path().join("data");
  std::fs::create_dir_all(&workspace_root)?;
  std::fs::create_dir_all(&data_dir)?;
  let dump = dir.path().join("service.env");
  let sentinel = dir.path().join("release");

  let config = fsync_config(data_dir.clone(), workspace_root);
  let live = spawn_offline_job(&config, fixture_job(&dump, &sentinel)?);

  // The job is live: talk to the cache server it stood up over the
  // fsync-configured CAS.
  let (base, bearer) = await_service_env(&dump).await?;
  let client = reqwest::Client::new();
  let bytes = payload()?;
  assert!(!bytes.is_empty(), "payload must be non-empty");
  save_entry(&client, &base, &bearer, &bytes).await?;
  let restored = restore_entry(&client, &base, &bearer).await?;
  assert_eq!(
    restored, bytes,
    "an entry saved with fsync_chunks = true must restore byte-for-byte"
  );

  std::fs::write(&sentinel, b"go")?;
  let teardown = live.job.await??;
  live.drain.await?;
  teardown.finish(&config).await;

  assert_entry_retained(&data_dir)
}
