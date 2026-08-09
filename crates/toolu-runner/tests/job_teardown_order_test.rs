//! T6/T7 (AC-10 / AC-11): the deferred-teardown ordering contract.
//!
//! Real data only — a real `Offline`-mode job over the committed
//! `job_message.json` fixture, a real CAS seeded by ingesting this workspace's
//! own `Cargo.lock`, and a real backdated workspace directory.
//!
//! Two halves, because the ordering is only *deterministically* observable
//! from the `run_job` side:
//!
//! 1. `run_job` hands back a `JobTeardown` with the job already completed and
//!    the event channel already closed, but with cache GC and the workspace
//!    sweep provably not yet run — the test itself owns the sender that closes
//!    the channel and decides when `finish` runs.
//! 2. `Runner::execute_job` — this is the ordering-focused half; see also
//!    `execute_job_error_reporting_test.rs` for its failure-reporting path —
//!    proves the restructured spawn still closes its channel (no deadlock) and
//!    still runs the teardown afterwards. It deliberately does **not** assert
//!    "GC has not run yet" at the instant the receiver yields `None`: there,
//!    the sender is dropped *inside* the spawned task, which then continues
//!    straight into `finish()` without yielding, so any such assertion is a
//!    race the test would lose as often as win.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use cache::cas::{CacheIndex, CasStore, IndexEntry};
use execution::Runner;
use execution::execution::job_runner::run_job;
use filetime::FileTime;
use shared::{
  ActionStep, AgentJobRequestMessage, RunnerConfig, RunnerEvent, SecretMasker, ServicesMode,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const JOB_MESSAGE: &str = include_str!("fixtures/job_message.json");

/// The permissive scope the offline cache server writes under.
const SCOPE: &str = "offline";
/// One opaque cache version for the seeded entry.
const VERSION: &str = "v-teardown-order";
/// Name of the stale workspace directory the sweep must remove.
const STALE_WORKSPACE: &str = "0000-stale-job";
/// How long a bounded poll waits for the deferred teardown to land. A real
/// pass over this fixture takes single-digit milliseconds; the margin is for a
/// loaded CI box, and it caps how long a regression takes to surface.
const TEARDOWN_TIMEOUT: Duration = Duration::from_secs(20);

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

/// A prepared job: the runner config, the CAS handles pointing at the same
/// on-disk root the job will use, and the two workspace paths under test.
struct Harness {
  /// Kept alive so the tempdir outlives the job.
  _dir: tempfile::TempDir,
  config: RunnerConfig,
  index: CacheIndex,
  store: CasStore,
  /// Backdated workspace of a previous job; the sweep must remove it.
  stale_workspace: PathBuf,
  /// The running job's own (also backdated) workspace; never removed.
  job_workspace: PathBuf,
  msg: AgentJobRequestMessage,
}

impl Harness {
  /// Number of live entries in the seeded cache index.
  fn index_len(&self) -> TestResult<usize> {
    Ok(self.index.records()?.len())
  }

  /// Whether cache GC has completed its full pass: the expired index entry is
  /// gone *and* its now-unreferenced manifest has been swept.
  fn cache_gc_done(&self) -> TestResult<bool> {
    Ok(self.index_len()? == 0 && self.store.list_manifest_ids()?.is_empty())
  }
}

/// Build the fixture job with a single trivial script step.
fn fixture_job() -> TestResult<AgentJobRequestMessage> {
  let mut msg: AgentJobRequestMessage = serde_json::from_str(JOB_MESSAGE)?;
  msg.steps = vec![ActionStep::script("noop", "echo teardown-order", "")];
  Ok(msg)
}

/// Back-date `path`'s mtime by `age` so the age-based sweeps consider it stale.
fn backdate(path: &Path, age: Duration) -> TestResult {
  let past = SystemTime::now()
    .checked_sub(age)
    .ok_or("cannot backdate before the epoch")?;
  filetime::set_file_mtime(path, FileTime::from_system_time(past))?;
  Ok(())
}

/// Ingest this workspace's real `Cargo.lock` into the job's CAS root and index
/// it under a `created_at` well past the default 7-day `entry_ttl_days`, so the
/// job's teardown GC has real work to do.
async fn seed_expired_entry(cache_dir: &Path) -> TestResult<(CasStore, CacheIndex)> {
  let store = CasStore::new(cache_dir.to_path_buf(), 16384, 1 << 30);
  let lock = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock");
  let manifest = store.ingest(&lock).await?;
  let manifest_id = store.put_manifest(&manifest).await?;

  let index = CacheIndex::new(cache_dir.to_path_buf());
  index.insert(
    SCOPE,
    VERSION,
    &IndexEntry {
      key: "expired-entry".to_owned(),
      manifest: manifest_id,
      size_bytes: manifest.total_size,
      created_at: chrono::Utc::now() - chrono::TimeDelta::days(30),
    },
  )?;
  Ok((store, index))
}

/// Stand up the tempdir layout, seed the expired cache entry, and pre-create
/// both workspaces backdated past the default 24 h `workspace_gc_hours`.
///
/// The running job's own workspace is backdated too, so the only thing that can
/// spare it is the sweep's `keep` guard — not a fresh mtime.
async fn setup() -> TestResult<Harness> {
  let dir = tempfile::tempdir()?;
  let workspace_root = dir.path().join("work");
  let data_dir = dir.path().join("data");
  std::fs::create_dir_all(&workspace_root)?;
  std::fs::create_dir_all(&data_dir)?;

  let (store, index) = seed_expired_entry(&data_dir.join("cache")).await?;

  let msg = fixture_job()?;
  let stale_age = Duration::from_secs(48 * 3600);
  let stale_workspace = workspace_root.join(STALE_WORKSPACE);
  std::fs::create_dir_all(&stale_workspace)?;
  std::fs::write(stale_workspace.join("leftover.txt"), b"from a previous job")?;
  backdate(&stale_workspace, stale_age)?;
  let job_workspace = workspace_root.join(&msg.job_id);
  std::fs::create_dir_all(&job_workspace)?;
  backdate(&job_workspace, stale_age)?;

  let config = RunnerConfig {
    data_dir,
    workspace_root,
    cgroup_path: None,
    services_mode: ServicesMode::Offline,
    ..RunnerConfig::default()
  };

  Ok(Harness {
    _dir: dir,
    config,
    index,
    store,
    stale_workspace,
    job_workspace,
    msg,
  })
}

/// Drain the engine event stream to close, returning whether the job reported
/// `JobCompleted`.
async fn drain_until_closed(rx: &mut mpsc::Receiver<RunnerEvent>) -> bool {
  let mut completed = false;
  while let Some(ev) = rx.recv().await {
    if matches!(ev, RunnerEvent::JobCompleted { .. }) {
      completed = true;
    }
  }
  completed
}

/// Assert the seeded entry is untouched: index record present, manifest on disk.
fn assert_entry_intact(fx: &Harness, when: &str) -> TestResult {
  assert_eq!(fx.index_len()?, 1, "cache GC must not have run {when}");
  assert!(
    !fx.store.list_manifest_ids()?.is_empty(),
    "the seeded manifest must survive {when}"
  );
  Ok(())
}

/// Assert the workspace sweep ran: stale directory pruned, running job spared.
fn assert_workspaces_swept(fx: &Harness) {
  assert!(
    !fx.stale_workspace.exists(),
    "the stale workspace must be pruned by the end of the job"
  );
  assert!(
    fx.job_workspace.exists(),
    "the running job's own workspace must never be pruned"
  );
}

/// Poll `ready` every 20 ms until it is true, failing after [`TEARDOWN_TIMEOUT`].
async fn wait_until(what: &str, mut ready: impl FnMut() -> TestResult<bool>) -> TestResult {
  let deadline = SystemTime::now()
    .checked_add(TEARDOWN_TIMEOUT)
    .ok_or("deadline overflow")?;
  loop {
    if ready()? {
      return Ok(());
    }
    if SystemTime::now() >= deadline {
      return Err(format!("timed out after {TEARDOWN_TIMEOUT:?} waiting for {what}").into());
    }
    tokio::time::sleep(Duration::from_millis(20)).await;
  }
}

/// AC-10 / AC-11, the deterministic half: `run_job` returns with the job
/// reported and its event channel closed, but with neither cache GC nor the
/// workspace sweep applied — both happen only in `JobTeardown::finish`.
#[tokio::test]
async fn run_job_defers_gc_until_finish() -> TestResult {
  let fx = setup().await?;
  assert_eq!(fx.index_len()?, 1, "seeded entry must be indexed");

  let masker = Arc::new(Mutex::new(SecretMasker::new()));
  let (tx, mut rx) = mpsc::channel::<RunnerEvent>(1024);
  // `tx` is moved into `run_job`, so the channel closes exactly when it returns.
  let teardown = run_job(
    fx.msg.clone(),
    &fx.config,
    CancellationToken::new(),
    tx,
    masker,
  )
  .await?;

  assert!(
    drain_until_closed(&mut rx).await,
    "the job must have reported JobCompleted"
  );

  // The receiver has yielded `None` — the channel is closed and the job is
  // reported — and the teardown is still in our hands, so GC provably has not
  // run. This is the whole point of T6.
  assert_entry_intact(&fx, "before the event channel closed")?;

  teardown.finish(&fx.config).await;

  assert!(
    fx.cache_gc_done()?,
    "finish() must expire the entry and sweep its unreferenced manifest"
  );
  // AC-11: finish() joins the workspace sweep spawned at job start.
  assert_workspaces_swept(&fx);
  Ok(())
}

/// AC-10 / AC-11 through the production entrypoint: `Runner::execute_job`'s
/// spawned task must close its event channel (the listener's completion
/// signal) and only then run the deferred teardown — and it must never drop
/// the `JobTeardown` on the floor.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn execute_job_closes_channel_then_runs_teardown() -> TestResult {
  let fx = setup().await?;
  assert_eq!(fx.index_len()?, 1, "seeded entry must be indexed");

  let runner = Runner::new(fx.config.clone(), Arc::new(Mutex::new(SecretMasker::new())));
  let mut rx = runner.execute_job(fx.msg.clone(), CancellationToken::new());

  // A bounded drain: if the restructured spawn ever left a sender alive, this
  // would hang forever instead of failing.
  let completed = tokio::time::timeout(TEARDOWN_TIMEOUT, drain_until_closed(&mut rx))
    .await
    .map_err(|elapsed| format!("the engine event channel never closed ({elapsed})"))?;
  assert!(completed, "the job must have reported JobCompleted");

  wait_until("cache GC to run after the channel closed", || {
    fx.cache_gc_done()
  })
  .await?;
  wait_until("the stale workspace to be pruned", || {
    Ok(!fx.stale_workspace.exists())
  })
  .await?;
  assert_workspaces_swept(&fx);
  Ok(())
}
