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
//!
//! The temp-cleanup cases pin the opposite order on purpose: `_temp` and `tmp`
//! are empty before the channel closes, because closing it lets the next job
//! start in the same `data_dir`.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use cache::cas::{CacheIndex, CasStore, IndexEntry};
use execution::Runner;
use execution::execution::job_runner::run_job;
use filetime::FileTime;
use shared::{
  ActionStep, AgentJobRequestMessage, Conclusion, RunnerConfig, RunnerEvent, SecretMasker,
  ServicesMode,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::gh_compat_node_action_file_commands::{seed_node, system_node};
use crate::run_service_classify_test::EventCapture;

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
    workspace: shared::WorkspaceConfig {
      mode: shared::WorkspaceMode::Fresh,
      stable_gc_hours: 0,
    },
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

/// What one job reported by the time its event channel closed.
struct Finished {
  conclusion: Option<Conclusion>,
  log: String,
}

/// Collect a job's stream until its channel closes.
async fn drain_job(rx: &mut mpsc::Receiver<RunnerEvent>) -> Finished {
  let mut finished = Finished {
    conclusion: None,
    log: String::new(),
  };
  while let Some(event) = rx.recv().await {
    if let RunnerEvent::JobCompleted { conclusion, .. } = event {
      finished.conclusion = Some(conclusion);
    } else if let RunnerEvent::Log { line, .. } = event {
      finished.log.push_str(&line);
    }
  }
  finished
}

/// Run `steps` through the production entrypoint until the channel closes.
async fn run_steps(fx: &Harness, steps: Vec<ActionStep>, cancel: CancellationToken) -> Finished {
  let msg = AgentJobRequestMessage {
    steps,
    ..fx.msg.clone()
  };
  let runner = Runner::new(fx.config.clone(), Arc::new(Mutex::new(SecretMasker::new())));
  drain_job(&mut runner.execute_job(msg, cancel)).await
}

fn temp_root(config: &RunnerConfig) -> PathBuf {
  config.data_dir.join("_temp")
}

/// Assert `_temp` and `tmp` hold nothing. A root replaced by a step may be gone.
fn assert_temp_empty(config: &RunnerConfig) -> TestResult {
  for root in [temp_root(config), config.data_dir.join("tmp")] {
    if root.exists() {
      let left: Vec<_> = std::fs::read_dir(&root)?
        .map(|entry| entry.map(|e| e.file_name()))
        .collect::<Result<_, _>>()?;
      assert!(left.is_empty(), "{} still holds {left:?}", root.display());
    }
  }
  Ok(())
}

/// A later step reads files earlier steps wrote, and only then are they gone.
#[tokio::test]
async fn temp_is_empty_at_channel_close_after_success_and_steps_read_it_first() -> TestResult {
  let fx = setup().await?;
  let steps = vec![
    ActionStep::script(
      "one",
      "echo temp-payload > \"$RUNNER_TEMP/payload\"\necho marker=kept >> \"$GITHUB_OUTPUT\"",
      "",
    ),
    ActionStep::script(
      "two",
      "cat \"$RUNNER_TEMP/payload\"\necho saw=${{ steps.one.outputs.marker }}",
      "",
    ),
  ];
  let job = run_steps(&fx, steps, CancellationToken::new()).await;
  assert_eq!(job.conclusion, Some(Conclusion::Success), "{}", job.log);
  assert!(job.log.contains("temp-payload"), "{}", job.log);
  assert!(job.log.contains("saw=kept"), "{}", job.log);
  assert_temp_empty(&fx.config)
}

#[tokio::test]
async fn temp_is_empty_at_channel_close_after_failure() -> TestResult {
  let fx = setup().await?;
  let step = ActionStep::script("fail", "touch \"$RUNNER_TEMP/written\"\nexit 1", "");
  let job = run_steps(&fx, vec![step], CancellationToken::new()).await;
  assert_eq!(job.conclusion, Some(Conclusion::Failure), "{}", job.log);
  assert_temp_empty(&fx.config)
}

#[tokio::test]
async fn temp_is_empty_at_channel_close_after_cancel() -> TestResult {
  let fx = setup().await?;
  let step = ActionStep::script("wait", "touch \"$RUNNER_TEMP/running\"\nsleep 300", "");
  let cancel = CancellationToken::new();
  let msg = AgentJobRequestMessage {
    steps: vec![step],
    ..fx.msg.clone()
  };
  let runner = Runner::new(fx.config.clone(), Arc::new(Mutex::new(SecretMasker::new())));
  let mut rx = runner.execute_job(msg, cancel.clone());
  let running = temp_root(&fx.config).join("running");
  wait_until("the step to start", || Ok(running.exists())).await?;
  cancel.cancel();
  let job = drain_job(&mut rx).await;
  assert_eq!(job.conclusion, Some(Conclusion::Cancelled), "{}", job.log);
  assert_temp_empty(&fx.config)
}

/// `run_job` returning `Err` reports a failed job, so it cleans up too. A
/// read-only root makes the start cleanup leave `leftover`; building the job
/// context restores the root, so only the end cleanup can remove it.
#[cfg(unix)]
#[tokio::test]
async fn temp_is_empty_at_channel_close_after_setup_error() -> TestResult {
  use std::os::unix::fs::PermissionsExt;
  let mut fx = setup().await?;
  let temp = temp_root(&fx.config);
  std::fs::create_dir_all(&temp)?;
  std::fs::write(temp.join("leftover"), "x")?;
  std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o500))?;
  // Directory creation fails under a regular file, after the job context is built.
  let blocked = fx.config.data_dir.join("not-a-directory");
  std::fs::write(&blocked, "x")?;
  fx.config.workspace_root = blocked;
  let job = run_steps(
    &fx,
    vec![ActionStep::script("noop", "true", "")],
    CancellationToken::new(),
  )
  .await;
  assert_eq!(job.conclusion, Some(Conclusion::Failure), "{}", job.log);
  assert_temp_empty(&fx.config)
}

/// The channel closing frees the next job, so `run_job` holds it open until temp
/// is empty even when the caller gave it the only sender.
#[tokio::test]
async fn run_job_keeps_the_channel_open_until_temp_is_empty() -> TestResult {
  let fx = setup().await?;
  let msg = AgentJobRequestMessage {
    steps: vec![ActionStep::script(
      "write",
      "touch \"$RUNNER_TEMP/written\"",
      "",
    )],
    ..fx.msg.clone()
  };
  let (tx, mut rx) = mpsc::channel::<RunnerEvent>(1024);
  let masker = Arc::new(Mutex::new(SecretMasker::new()));
  let (result, at_close) = tokio::join!(
    run_job(msg, &fx.config, CancellationToken::new(), tx, masker),
    async {
      drain_job(&mut rx).await;
      assert_temp_empty(&fx.config).map_err(|error| error.to_string())
    }
  );
  result?.finish(&fx.config).await;
  at_close?;
  Ok(())
}

/// The next job may start as soon as the channel closes, so `finish` must never
/// delete what it writes, however long cache GC takes.
#[tokio::test]
async fn finish_never_touches_files_of_the_next_job() -> TestResult {
  let fx = setup().await?;
  let (tx, mut rx) = mpsc::channel::<RunnerEvent>(1024);
  let masker = Arc::new(Mutex::new(SecretMasker::new()));
  let teardown = run_job(
    fx.msg.clone(),
    &fx.config,
    CancellationToken::new(),
    tx,
    masker,
  )
  .await?;
  drain_job(&mut rx).await;
  assert_temp_empty(&fx.config)?;

  let next_job_file = temp_root(&fx.config).join("next-job");
  std::fs::write(&next_job_file, "live")?;
  teardown.finish(&fx.config).await;
  assert!(next_job_file.exists(), "finish deleted a live file");
  Ok(())
}

/// A post step runs after every main step and can still read `$RUNNER_TEMP`.
#[tokio::test]
async fn post_step_reads_temp_before_it_is_emptied() -> TestResult {
  let node = system_node().ok_or("this test needs a real `node` on PATH")?;
  let fx = setup().await?;
  seed_node(&fx.config.data_dir, &node)?;
  let copied = fx.config.data_dir.join("post-copied");
  let action = fx.job_workspace.join("post-reader");
  std::fs::create_dir_all(&action)?;
  std::fs::write(
    action.join("action.yml"),
    "name: post-reader\ndescription: d\nruns:\n  using: node20\n  main: main.js\n  post: post.js\n",
  )?;
  let payload = r#"require("path").join(process.env.RUNNER_TEMP, "from-main")"#;
  std::fs::write(
    action.join("main.js"),
    format!(r#"require("fs").writeFileSync({payload}, "from-main");"#),
  )?;
  std::fs::write(
    action.join("post.js"),
    format!(
      r#"require("fs").copyFileSync({payload}, {:?});"#,
      copied.display().to_string()
    ),
  )?;
  let mut step = ActionStep::with_ref_type("reader", "repository");
  step.reference.name = Some("./post-reader".to_owned());

  let job = run_steps(&fx, vec![step], CancellationToken::new()).await;
  assert_eq!(job.conclusion, Some(Conclusion::Success), "{}", job.log);
  assert_eq!(std::fs::read_to_string(&copied)?, "from-main");
  assert_temp_empty(&fx.config)
}

/// Links are removed and never followed, at any depth or as a root itself.
#[tokio::test]
async fn symlinks_are_removed_and_their_targets_are_kept() -> TestResult {
  let fx = setup().await?;
  let outside = fx.config.data_dir.join("outside");
  std::fs::create_dir_all(outside.join("dir"))?;
  std::fs::write(outside.join("file"), "keep")?;
  std::fs::write(outside.join("dir/inner"), "keep")?;
  let out = outside.display();
  let links = format!(
    "ln -s {out}/file \"$RUNNER_TEMP/file-link\"\nln -s {out}/dir \"$RUNNER_TEMP/dir-link\"\n\
     mkdir \"$RUNNER_TEMP/sub\"\nln -s {out}/dir \"$RUNNER_TEMP/sub/link\"\n"
  );
  let job = run_steps(
    &fx,
    vec![ActionStep::script("links", &links, "")],
    CancellationToken::new(),
  )
  .await;
  assert_eq!(job.conclusion, Some(Conclusion::Success), "{}", job.log);
  assert_temp_empty(&fx.config)?;

  let swap = format!("rm -rf \"$RUNNER_TEMP\"\nln -s {out}/dir \"$RUNNER_TEMP\"\n");
  let job = run_steps(
    &fx,
    vec![ActionStep::script("swap", &swap, "")],
    CancellationToken::new(),
  )
  .await;
  assert_eq!(job.conclusion, Some(Conclusion::Success), "{}", job.log);
  assert!(
    std::fs::symlink_metadata(temp_root(&fx.config)).is_err(),
    "the replaced root must be unlinked"
  );
  assert_eq!(std::fs::read_to_string(outside.join("file"))?, "keep");
  assert_eq!(std::fs::read_to_string(outside.join("dir/inner"))?, "keep");
  Ok(())
}

/// A tree a step left read-only (Go's module cache does) does not stay forever.
#[tokio::test]
async fn read_only_trees_are_removed() -> TestResult {
  let fx = setup().await?;
  let script = "mkdir -p \"$RUNNER_TEMP/ro/sub\"\ntouch \"$RUNNER_TEMP/ro/sub/file\"\n\
                chmod -R a-w \"$RUNNER_TEMP/ro\"";
  let job = run_steps(
    &fx,
    vec![ActionStep::script("ro", script, "")],
    CancellationToken::new(),
  )
  .await;
  assert_eq!(job.conclusion, Some(Conclusion::Success), "{}", job.log);
  assert_temp_empty(&fx.config)
}

/// An undeletable file warns, spares nothing else, keeps the job's conclusion,
/// and does not stop the next job, which clears it once it can.
#[cfg(unix)]
#[tokio::test]
async fn undeletable_file_warns_and_never_changes_the_result() -> TestResult {
  use std::os::unix::fs::PermissionsExt;
  let fx = setup().await?;
  // Root ignores mode bits; probe and skip in that case.
  let probe = fx.config.data_dir.join("probe");
  std::fs::create_dir(&probe)?;
  std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o500))?;
  if std::fs::write(probe.join("file"), "x").is_ok() {
    eprintln!("skipping: running as root, cannot make a directory unwritable");
    return Ok(());
  }

  // The root itself is the one directory cleanup never makes writable.
  let script = "mkdir \"$RUNNER_TEMP/sub\"\ntouch \"$RUNNER_TEMP/sub/ok\" \"$RUNNER_TEMP/stuck\"\n\
                chmod 500 \"$RUNNER_TEMP\"";
  let msg = AgentJobRequestMessage {
    steps: vec![ActionStep::script("lock", script, "")],
    ..fx.msg.clone()
  };
  let capture = EventCapture::default();
  let (tx, mut rx) = mpsc::channel::<RunnerEvent>(1024);
  let masker = Arc::new(Mutex::new(SecretMasker::new()));
  let result = {
    let _guard = tracing::subscriber::set_default(capture.clone());
    run_job(msg, &fx.config, CancellationToken::new(), tx, masker).await
  };
  let job = drain_job(&mut rx).await;
  result?.finish(&fx.config).await;

  let temp = temp_root(&fx.config);
  assert_eq!(job.conclusion, Some(Conclusion::Success), "{}", job.log);
  assert!(capture.saw(
    tracing::Level::WARN,
    "could not empty runner temp directory"
  ));
  assert!(
    temp.join("stuck").exists(),
    "the fixture must really be undeletable"
  );
  assert!(
    !temp.join("sub/ok").exists(),
    "other files must still be removed"
  );

  let next = run_steps(
    &fx,
    vec![ActionStep::script("next", "true", "")],
    CancellationToken::new(),
  )
  .await;
  assert_eq!(next.conclusion, Some(Conclusion::Success), "{}", next.log);
  assert_temp_empty(&fx.config)
}
