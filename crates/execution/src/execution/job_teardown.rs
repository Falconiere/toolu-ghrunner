//! Work a finished job defers past its own completion: cache maintenance over
//! the retained CAS handles, and the workspace sweep spawned at job start.
//!
//! Everything here is deliberately best-effort — a failure is logged and the
//! job's already-reported conclusion stands.

use std::path::{Path, PathBuf};
use std::time::Duration;

use cache::blob::sweep_staging;
use cache::cas::{CacheGc, CacheIndex, CasStore, LeaseSet};
use shared::RunnerConfig;
use tokio::task::JoinHandle;
use tracing::info;

/// The shared CAS handles a job's local cache server was built over, retained
/// so [`JobTeardown::finish`] can sweep staging and GC the store at teardown.
///
/// Every handle is a cheap clone onto the same on-disk root / in-memory lease
/// map the running server holds, so GC honours any in-flight read lease.
/// Not exercised today — `run_job`'s ordering contract (see [`JobTeardown`])
/// drains every lease before `finish` runs GC — but it keeps `CacheGc::run`'s
/// lease-safety contract true regardless of when a future caller runs it.
pub(super) struct CacheMaintenance {
  /// Content-addressed store the local server ingested into.
  pub(super) store: CasStore,
  /// Persistent `(scope, version)` entry index GC rewrites.
  pub(super) index: CacheIndex,
  /// Live read leases GC must not delete chunks out from under.
  pub(super) leases: LeaseSet,
  /// `cas/staging` root holding in-flight uploads.
  pub(super) staging_root: PathBuf,
}

/// Age past which an abandoned `cas/staging` upload is swept at job teardown.
///
/// Comfortably exceeds a single upload window; single-job concurrency (the
/// `.lock`) means nothing else is mid-upload when teardown runs.
const STAGING_SWEEP_TTL: Duration = Duration::from_secs(3600);

/// The deferred teardown of one job, returned by `job_runner::run_job`.
///
/// **Ordering contract.** `run_job` stops the local cache servers and emits
/// `JobCompleted` *before* returning this value, and the caller must drop the
/// job's event sender — closing the engine event channel, which is what
/// ultimately reports the job to GitHub — *before* calling
/// [`finish`](Self::finish). Cache GC reads every scope index and walks every
/// manifest and chunk on disk; running it any earlier puts it squarely between
/// "job done" and "GitHub told".
///
/// `#[must_use]`: dropping this silently skips maintenance. `Drop` backs it
/// with a runtime WARN for the `let _ = teardown;` case `#[must_use]` misses.
#[must_use]
pub struct JobTeardown {
  /// Retained CAS handles, or `None` in `Forwarder` mode (no local server).
  maintenance: Option<CacheMaintenance>,
  /// The workspace sweep spawned at job start, or `None` when
  /// `workspace_gc_hours == 0` disables it.
  workspace_gc: Option<JoinHandle<()>>,
  /// Set by `finish` so `Drop` can tell "already ran" apart from "skipped".
  finished: bool,
}

impl JobTeardown {
  /// Package the deferred work of a finished job.
  pub(super) fn new(
    maintenance: Option<CacheMaintenance>,
    workspace_gc: Option<JoinHandle<()>>,
  ) -> Self {
    Self {
      maintenance,
      workspace_gc,
      finished: false,
    }
  }

  /// Run the deferred teardown: cache staging sweep + one GC pass, then join
  /// the workspace sweep (joined last — it started at job start and is
  /// almost always already finished).
  ///
  /// **Ordering contract:** call only after the job's event sender is
  /// dropped (see the [type docs](Self)), so GC overlaps GitHub's
  /// completion round-trip instead of preceding it.
  ///
  /// Best-effort: errors and a panicked/cancelled sweep are WARNed and
  /// swallowed. The `JoinError` arm is untested — it needs an injected task
  /// panic, which this suite has no hook for.
  pub async fn finish(mut self, config: &RunnerConfig) {
    self.finished = true;
    if let Some(maintenance) = &self.maintenance {
      run_cache_maintenance(config, maintenance).await;
    }
    if let Some(handle) = self.workspace_gc.take()
      && let Err(e) = handle.await
    {
      tracing::warn!(error = %e, "workspace GC task panicked or was cancelled; continuing");
    }
  }
}

impl Drop for JobTeardown {
  /// WARN when dropped without `finish` ever running — the `let _ =
  /// teardown;` case `#[must_use]` can't catch on its own. The workspace-GC
  /// task itself is unaffected (Tokio never cancels a dropped `JoinHandle`);
  /// only the staging sweep + cache GC pass are skipped.
  fn drop(&mut self) {
    if !self.finished {
      tracing::warn!(
        "JobTeardown dropped without calling finish(); cache staging sweep and cache GC \
         were skipped for this job"
      );
    }
  }
}

/// Best-effort cache teardown: sweep abandoned staging uploads, then run one GC
/// pass (TTL expiry + `max_bytes` eviction + unreferenced-blob sweep).
///
/// Neither step may fail the job: errors are logged and swallowed. Runs after
/// the server has stopped, so no handler holds a read lease GC must respect.
async fn run_cache_maintenance(config: &RunnerConfig, maintenance: &CacheMaintenance) {
  sweep_staging_best_effort(maintenance.staging_root.clone()).await;
  gc_best_effort(config, maintenance).await;
}

/// Sweep abandoned `cas/staging` uploads best-effort, on the blocking pool.
///
/// `sweep_staging` is synchronous `read_dir`/`remove_dir_all` filesystem
/// work, so it must not run on the async worker thread `finish` is called
/// from.
async fn sweep_staging_best_effort(staging_root: PathBuf) {
  let joined = tokio::task::spawn_blocking(move || log_staging_sweep(&staging_root)).await;
  if let Err(e) = joined {
    tracing::warn!(error = %e, "cache staging sweep task did not complete; continuing");
  }
}

/// The synchronous sweep, with best-effort logging; runs inside
/// [`sweep_staging_best_effort`]'s `spawn_blocking`.
fn log_staging_sweep(staging_root: &Path) {
  match sweep_staging(staging_root, STAGING_SWEEP_TTL) {
    Ok(0) => {},
    Ok(n) => info!(removed = n, "cache staging sweep removed abandoned uploads"),
    Err(e) => tracing::warn!(error = %e, "cache staging sweep failed; continuing"),
  }
}

/// Run one GC pass over the retained CAS handles best-effort; never fails the job.
async fn gc_best_effort(config: &RunnerConfig, maintenance: &CacheMaintenance) {
  let gc = CacheGc::new(config.cache.entry_ttl_days, config.cache.max_bytes);
  match gc
    .run(&maintenance.store, &maintenance.index, &maintenance.leases)
    .await
  {
    Ok(report) => info!(?report, "cache GC pass complete"),
    Err(e) => tracing::warn!(error = %e, "cache GC failed; continuing"),
  }
}
