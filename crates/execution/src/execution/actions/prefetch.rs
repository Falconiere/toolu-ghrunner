//! Single-flight action tarball prefetcher (S6).
//!
//! [`ActionFetcher`] dedupes concurrent downloads of the SAME remote action
//! ref: the first `ensure` call for a given cache key runs the download, and
//! any concurrent caller for that same key awaits the same in-flight
//! attempt instead of starting a second one. [`spawn_prefetch`] kicks a
//! background job-start prefetch of every distinct top-level remote `uses:`
//! ref (bounded concurrency), so step-time resolution
//! (`action_exec::resolve_remote_action`, which routes through the SAME
//! fetcher) usually finds the tarball already on disk.
//!
//! Failure semantics (spec-pinned): a failed `ensure` call evicts its
//! single-flight entry, so the NEXT `ensure` for that key starts a fresh
//! download instead of replaying the failure forever. A background
//! prefetch's failure is WARN-logged and swallowed here — it never fails
//! the job; step-time resolution simply retries what prefetch failed to
//! fetch.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use futures_util::StreamExt;
use futures_util::stream;
use shared::{ActionStep, RunnerError};
use tokio::sync::OnceCell;
use tokio::task::JoinHandle;

use super::downloader::{action_cache_dir, download_and_extract_action};
use super::resolver::resolve_action_refs;
use crate::execution::action_exec::build_uses_ref;

/// Concurrency cap for job-start action prefetch (Open Question 3 in the
/// design spec: revisit only with evidence).
const PREFETCH_CONCURRENCY: usize = 4;

/// The real GitHub API base every production prefetch/download call resolves
/// tarball URLs against. `prefetch_refs` takes the base as a parameter (this
/// constant is the only production value it is ever called with) so its
/// tests can point the SAME dedupe + fetch path at a local HTTP server
/// instead of `api.github.com`.
const GITHUB_API_BASE: &str = "https://api.github.com";

/// Single-flight download cache, keyed by the resolved action ref's
/// `{owner}/{repo}/{ref}` cache key
/// ([`super::resolver::ActionRef::cache_key`]). One instance is created per
/// job ([`crate::execution::job_runner::run_job`]) and shared by the
/// job-start prefetch and every step-time `uses:` resolution, so a step
/// waiting on `ensure` joins a prefetch already in flight for the same ref
/// instead of downloading it again.
#[derive(Default)]
pub struct ActionFetcher {
  inflight: Mutex<HashMap<String, Arc<OnceCell<PathBuf>>>>,
}

impl ActionFetcher {
  /// Create an empty fetcher (one per job).
  #[must_use]
  pub fn new() -> Self {
    Self::default()
  }

  /// Ensure the action at `cache_key` is downloaded and extracted, returning
  /// its cache directory (`action_cache_dir(data_dir, cache_key)`); a no-op
  /// if already cached (the watermark check inside
  /// [`download_and_extract_action`]).
  ///
  /// The first caller for `cache_key` runs the download; a concurrent
  /// `ensure` call for the same key awaits that same in-flight attempt
  /// instead of starting a second one and gets back the same `PathBuf`. A
  /// failed attempt evicts the key's single-flight entry so the next
  /// `ensure` call — from prefetch or from step-time resolution — retries
  /// fresh rather than replaying the failure.
  ///
  /// # Errors
  ///
  /// Returns `RunnerError::ActionDownload` when the download or extraction
  /// fails.
  pub async fn ensure(
    &self,
    client: &reqwest::Client,
    cache_key: &str,
    tarball_url: &str,
    cache_dir: &Path,
  ) -> Result<PathBuf, RunnerError> {
    let cell = self.cell_for(cache_key);
    let dest = cache_dir.to_path_buf();
    let result = cell
      .get_or_try_init(move || async move {
        download_and_extract_action(client, tarball_url, None, &dest).await?;
        Ok::<PathBuf, RunnerError>(dest)
      })
      .await;
    match result {
      Ok(path) => Ok(path.clone()),
      Err(err) => {
        self.evict(cache_key);
        Err(err)
      },
    }
  }

  /// Fetch (creating if absent) the shared single-flight cell for `cache_key`.
  fn cell_for(&self, cache_key: &str) -> Arc<OnceCell<PathBuf>> {
    let mut guard = match self.inflight.lock() {
      Ok(g) => g,
      Err(poisoned) => poisoned.into_inner(),
    };
    Arc::clone(
      guard
        .entry(cache_key.to_owned())
        .or_insert_with(|| Arc::new(OnceCell::new())),
    )
  }

  /// Drop `cache_key`'s single-flight entry so the next `ensure` call for it
  /// starts a fresh download attempt instead of reusing a failed one.
  fn evict(&self, cache_key: &str) {
    let mut guard = match self.inflight.lock() {
      Ok(g) => g,
      Err(poisoned) => poisoned.into_inner(),
    };
    guard.remove(cache_key);
  }
}

/// Kick job-start action prefetch as a background task, returning its handle
/// so the caller can `.abort()` it once the job's step loop is done. Abort
/// stops any *future* fetch this task would otherwise start; it does NOT —
/// and cannot — cancel an already-running `spawn_blocking(extract_tarball)`
/// closure, which keeps executing to completion on its blocking thread even
/// after the `JoinHandle` is aborted (aborting a task only cancels it at its
/// next `.await` point, and a blocking closure has none). That is safe here:
/// `download_and_extract_action` extracts into a private staging directory
/// and promotes it onto the shared cache dir with a single atomic rename
/// (see `downloader::promote_staging`), so an extraction that outlives its
/// job can complete in the background without any other job or step ever
/// observing a partially-extracted tree at the cache path — worst case it's
/// redundant work, never corruption. The deduped remote-ref set is built
/// synchronously here (cheap, no I/O: `resolve_action_refs` over the job's
/// top-level `uses:` steps) so the spawned task owns only `'static` data.
pub fn spawn_prefetch(
  steps: &[ActionStep],
  fetcher: Arc<ActionFetcher>,
  client: reqwest::Client,
  data_dir: PathBuf,
) -> JoinHandle<()> {
  let uses_refs = top_level_uses_refs(steps);
  tokio::spawn(async move {
    prefetch_refs(&fetcher, &client, &data_dir, GITHUB_API_BASE, &uses_refs).await;
  })
}

/// Collect the `uses:` string for every non-`run:` (action) step. Local and
/// remote refs are both included — [`resolve_action_refs`] is what skips
/// local refs and dedupes remote ones, so this stays a plain filter+map.
fn top_level_uses_refs(steps: &[ActionStep]) -> Vec<String> {
  steps
    .iter()
    .filter(|step| !step.is_run_step())
    .map(|step| build_uses_ref(&step.reference))
    .collect()
}

/// Resolve `uses_refs` to their distinct remote actions (via
/// [`resolve_action_refs`] — the existing dedupe helper, reused rather than
/// reinvented) and prefetch each through `fetcher`, at most
/// [`PREFETCH_CONCURRENCY`] downloads in flight. `api_base` is a parameter
/// rather than hardcoded so this exact function can be driven against a
/// local HTTP server in tests; [`spawn_prefetch`] always calls it with
/// [`GITHUB_API_BASE`].
///
/// A malformed ref or a per-ref download failure is WARN-logged and
/// swallowed here — this function never fails the job; step-time resolution
/// (through the same `fetcher`) is what actually needs the tarball on disk.
async fn prefetch_refs(
  fetcher: &ActionFetcher,
  client: &reqwest::Client,
  data_dir: &Path,
  api_base: &str,
  uses_refs: &[String],
) {
  let resolved = match resolve_action_refs(uses_refs) {
    Ok(resolved) => resolved,
    Err(err) => {
      tracing::warn!(
        error = %err,
        "action prefetch: ref resolution failed; step-time resolution will retry"
      );
      return;
    },
  };

  stream::iter(resolved.into_values())
    .for_each_concurrent(PREFETCH_CONCURRENCY, |action| async move {
      let cache_key = action.action_ref.cache_key();
      let cache_dir = action_cache_dir(data_dir, &cache_key);
      let tarball_url = action.action_ref.tarball_url(api_base);
      if let Err(err) = fetcher
        .ensure(client, &cache_key, &tarball_url, &cache_dir)
        .await
      {
        tracing::warn!(
          action = cache_key.as_str(),
          error = %err,
          "action prefetch failed; step-time resolution will retry"
        );
      }
    })
    .await;
}

#[cfg(test)]
#[path = "tests/prefetch.rs"]
mod tests;
