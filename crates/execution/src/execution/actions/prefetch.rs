//! Per-job action prefetch and revision-qualified single-flight downloads.
//!
//! Prefetch and step execution share one fetcher. Each resolves the action
//! against the acquired job service; concurrent downloads of one revision
//! join the same attempt. Failed prefetches are logged, then retried at step
//! time without failing the job early.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use futures_util::StreamExt;
use futures_util::stream;
use shared::{ActionStep, AgentJobRequestMessage, RunnerError, SecretMasker};
use tokio::sync::OnceCell;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::download_info::ActionDownloadContext;
use super::downloader::{action_cache_dir, download_and_extract_action};
use super::resolver::{ActionRef, resolve_action_refs};
use crate::execution::action_exec::build_uses_ref;

/// Concurrency cap for job-start action prefetch (Open Question 3 in the
/// design spec: revisit only with evidence).
const PREFETCH_CONCURRENCY: usize = 4;
const ARCHIVE_ATTEMPTS: usize = 2;

/// Per-job fetcher shared by prefetch and step execution, keyed by resolved
/// action revision and API host.
#[derive(Default)]
pub struct ActionFetcher {
  inflight: Mutex<HashMap<String, Arc<OnceCell<PathBuf>>>>,
  context: Option<Result<ActionDownloadContext, String>>,
  masker: Option<Arc<Mutex<SecretMasker>>>,
  cancel: Option<CancellationToken>,
}

impl ActionFetcher {
  /// Create an empty fetcher for direct archive tests and local-only jobs.
  #[must_use]
  pub fn new() -> Self {
    Self::default()
  }

  /// Build the production fetcher from the acquired job and its shared masker.
  ///
  /// Context validation is deferred until a remote action needs it, so
  /// shell-only jobs do not require GitHub action-service fields.
  #[must_use]
  pub fn for_job(
    msg: &AgentJobRequestMessage,
    masker: Arc<Mutex<SecretMasker>>,
    cancel: CancellationToken,
  ) -> Self {
    Self {
      inflight: Mutex::new(HashMap::new()),
      context: Some(
        ActionDownloadContext::from_message(msg).map_err(|error| action_context_error(&error)),
      ),
      masker: Some(masker),
      cancel: Some(cancel),
    }
  }

  /// Resolve one action with the job's service, then fetch its exact revision.
  /// A failed archive authorization is retried once. Concurrent callers share
  /// one download, and cancellation stops a caller waiting for that download.
  ///
  /// # Errors
  ///
  /// Returns a sanitized resolution or archive failure.
  pub async fn ensure_action(
    &self,
    client: &reqwest::Client,
    action: &ActionRef,
    data_dir: &Path,
  ) -> Result<PathBuf, RunnerError> {
    for attempt in 0..ARCHIVE_ATTEMPTS {
      let result = self.ensure_action_once(client, action, data_dir).await;
      if attempt + 1 == ARCHIVE_ATTEMPTS || !result.as_ref().is_err_and(is_archive_auth_error) {
        return result;
      }
    }
    Err(RunnerError::ActionDownload(
      "archive credential refresh exhausted".to_owned(),
    ))
  }

  async fn ensure_action_once(
    &self,
    client: &reqwest::Client,
    action: &ActionRef,
    data_dir: &Path,
  ) -> Result<PathBuf, RunnerError> {
    let context = match self.context.as_ref() {
      Some(Ok(context)) => context,
      Some(Err(message)) => return Err(RunnerError::ActionResolution(message.clone())),
      None => {
        return Err(RunnerError::ActionDownload(
          "action fetcher has no acquired job".to_owned(),
        ));
      },
    };
    let masker = self.masker.as_ref().ok_or_else(|| {
      RunnerError::ActionDownload("action fetcher has no secret masker".to_owned())
    })?;
    let cancel = self.cancel.as_ref().ok_or_else(|| {
      RunnerError::ActionDownload("action fetcher has no cancellation token".to_owned())
    })?;
    let info = context.resolve_info(client, action, masker, cancel).await?;
    let dest = action_cache_dir(data_dir, &info.cache_key);
    let cell = self.cell_for(&info.cache_key);
    let result = tokio::select! {
      () = cancel.cancelled() => Err(RunnerError::ActionDownload("action download cancelled".to_owned())),
      fetched = cell.get_or_try_init(|| async {
        download_and_extract_action(client, &info.tarball_url, info.token.as_deref(), &dest)
          .await?;
        Ok::<PathBuf, RunnerError>(dest)
      }) => fetched,
    };
    match result {
      Ok(path) => Ok(path.clone()),
      Err(error) => {
        self.evict(&info.cache_key);
        Err(error)
      },
    }
  }

  /// Download a supplied archive URL once per key. Used by direct archive
  /// callers; production remote `uses:` steps call [`Self::ensure_action`].
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

  /// Drop `cache_key`'s single-flight entry after a failed attempt.
  ///
  /// This is NOT what makes the next `ensure` retry — a failed
  /// `OnceCell::get_or_try_init` already leaves the cell uninitialized, so
  /// the next caller re-runs the download through the very same cell. What
  /// eviction buys is freeing the map slot for a key that may never be asked
  /// about again (a one-off failing ref), keeping the map to live entries.
  fn evict(&self, cache_key: &str) {
    let mut guard = match self.inflight.lock() {
      Ok(g) => g,
      Err(poisoned) => poisoned.into_inner(),
    };
    guard.remove(cache_key);
  }
}

fn action_context_error(error: &RunnerError) -> String {
  if let RunnerError::ActionResolution(message) = error {
    message.clone()
  } else {
    error.to_string()
  }
}

fn is_archive_auth_error(error: &RunnerError) -> bool {
  if let RunnerError::ActionDownload(message) = error {
    message.starts_with("archive status 401") || message.starts_with("archive status 403")
  } else {
    false
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
    prefetch_job_refs(&fetcher, &client, &data_dir, &uses_refs).await;
  })
}

async fn prefetch_job_refs(
  fetcher: &ActionFetcher,
  client: &reqwest::Client,
  data_dir: &Path,
  uses_refs: &[String],
) {
  let resolved = match resolve_action_refs(uses_refs) {
    Ok(resolved) => resolved,
    Err(error) => {
      tracing::warn!(error = %error, "action prefetch ref resolution failed");
      return;
    },
  };
  stream::iter(resolved.into_values()).for_each_concurrent(PREFETCH_CONCURRENCY, |item| async move {
    if let Err(error) = fetcher.ensure_action(client, &item.action_ref, data_dir).await {
      tracing::warn!(action = %item.action_ref.cache_key(), error = %error, "action prefetch failed");
    }
  }).await;
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

#[cfg(test)]
#[path = "tests/prefetch.rs"]
mod tests;
