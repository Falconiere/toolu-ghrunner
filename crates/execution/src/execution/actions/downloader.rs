use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use flate2::read::GzDecoder;
use futures_util::StreamExt;
use shared::RunnerError;
use tar::Archive;

/// Cache directory for an action: `{data_dir}/actions/{cache_key}`.
pub fn action_cache_dir(data_dir: &Path, cache_key: &str) -> PathBuf {
  data_dir.join("actions").join(cache_key)
}

/// Watermark file path: `{cache_dir}.completed`.
pub fn watermark_path(cache_dir: &Path) -> PathBuf {
  let mut wm = cache_dir.as_os_str().to_owned();
  wm.push(".completed");
  PathBuf::from(wm)
}

/// Check whether an action is cached and valid (watermark + dir exist).
pub fn is_action_cached(cache_dir: &Path) -> bool {
  watermark_path(cache_dir).exists() && cache_dir.is_dir()
}

/// Extract a tar.gz tarball, stripping the GitHub prefix directory.
///
/// GitHub tarballs contain `{owner}-{repo}-{sha}/` as top-level dir.
/// This strips that first component so files land directly in `dest/`.
///
/// Takes a plain [`Read`] rather than a buffered byte slice so the caller
/// can stream the tarball body straight off the network (via
/// `tokio_util::io::SyncIoBridge`) instead of holding the whole tarball in
/// memory. This is CPU-bound, synchronous work — every production call
/// site MUST run it inside `tokio::task::spawn_blocking`.
///
/// # Errors
///
/// Returns `RunnerError::ActionDownload` on extraction failures.
pub fn extract_tarball(reader: impl Read, dest: &Path) -> Result<(), RunnerError> {
  std::fs::create_dir_all(dest)
    .map_err(|e| RunnerError::ActionDownload(format!("mkdir {}: {e}", dest.display())))?;

  let decoder = GzDecoder::new(reader);
  let mut archive = Archive::new(decoder);

  let entries = archive
    .entries()
    .map_err(|e| RunnerError::ActionDownload(format!("tar entries: {e}")))?;

  for entry_result in entries {
    let mut entry =
      entry_result.map_err(|e| RunnerError::ActionDownload(format!("tar entry: {e}")))?;

    let path = entry
      .path()
      .map_err(|e| RunnerError::ActionDownload(format!("entry path: {e}")))?
      .into_owned();

    // Strip the first component (GitHub's prefix directory)
    let components: Vec<_> = path.components().collect();
    if components.len() <= 1 {
      continue;
    }

    let stripped: PathBuf = components.get(1..).unwrap_or_default().iter().collect();
    reject_tar_slip(&path, &stripped)?;
    let target = dest.join(&stripped);

    if let Some(parent) = target.parent() {
      std::fs::create_dir_all(parent)
        .map_err(|e| RunnerError::ActionDownload(format!("mkdir: {e}")))?;
    }

    extract_entry(&mut entry, &target)?;
  }

  Ok(())
}

/// Reject an entry whose stripped path would escape `dest` when joined.
///
/// A malicious or compromised action can include `../` components in entry
/// paths. Joining those under `dest` would escape the cache directory (and
/// any file the runner user can write). Rejects any entry whose stripped
/// path contains a parent-dir, root, or (Windows) drive prefix component
/// before touching the filesystem.
fn reject_tar_slip(path: &Path, stripped: &Path) -> Result<(), RunnerError> {
  if stripped.components().any(|c| {
    matches!(
      c,
      std::path::Component::ParentDir
        | std::path::Component::RootDir
        | std::path::Component::Prefix(_)
    )
  }) {
    // Quoted deliberately: the old `{path:?}` rendering quoted it, and this
    // message is surfaced through RunnerError to logs. Quotes also keep a
    // path with trailing whitespace legible in the rejection record.
    return Err(RunnerError::ActionDownload(format!(
      "tar slip: entry \"{}\" escapes dest",
      path.display()
    )));
  }
  Ok(())
}

/// Write a single tar entry (dir or file) to `target`.
fn extract_entry(entry: &mut tar::Entry<'_, impl Read>, target: &Path) -> Result<(), RunnerError> {
  if entry.header().entry_type().is_dir() {
    std::fs::create_dir_all(target)
      .map_err(|e| RunnerError::ActionDownload(format!("mkdir: {e}")))?;
  } else {
    let mut content = Vec::new();
    entry
      .read_to_end(&mut content)
      .map_err(|e| RunnerError::ActionDownload(format!("read entry: {e}")))?;
    std::fs::write(target, &content)
      .map_err(|e| RunnerError::ActionDownload(format!("write: {e}")))?;

    #[cfg(unix)]
    set_executable_if_needed(target, entry);
  }
  Ok(())
}

#[cfg(unix)]
fn set_executable_if_needed(target: &Path, entry: &tar::Entry<'_, impl Read>) {
  if let Ok(mode) = entry.header().mode()
    && mode & 0o111 != 0
  {
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::Permissions::from_mode(mode);
    let _ = std::fs::set_permissions(target, perms);
  }
}

/// Write the watermark file to mark an action cache as complete.
///
/// Idempotent — safe to call even when the watermark already exists (a
/// concurrent extraction that lost the [`promote_staging`] race still calls
/// this after treating `cache_dir` as authoritative, so two writers racing
/// here just write the same empty file twice).
///
/// # Errors
///
/// Returns `RunnerError::ActionDownload` on filesystem failure.
pub fn write_watermark(cache_dir: &Path) -> Result<(), RunnerError> {
  let wm = watermark_path(cache_dir);
  std::fs::write(&wm, b"")
    .map_err(|e| RunnerError::ActionDownload(format!("watermark {}: {e}", wm.display())))
}

/// Monotonic counter disambiguating staging dirs created by this process.
/// Combined with the PID, this makes [`staging_dir_for`] collision-free
/// across both concurrent extractions in this process and concurrent
/// runner processes on the same host (each has a distinct PID) without
/// pulling in a UUID dependency.
static STAGING_SEQ: AtomicU64 = AtomicU64::new(0);

/// Build a unique sibling staging directory for `dest`:
/// `<dest>.tmp-<pid>-<seq>`. Extraction lands here first; [`promote_staging`]
/// renames it onto `dest` as the last step, so `dest` — the path every other
/// job/step reads through [`is_action_cached`] / [`action_cache_dir`] — never
/// observes a partially-extracted tree, even if this extraction is aborted
/// mid-flight (e.g. the job that started it ends and the prefetch task is
/// dropped/aborted around the still-running `spawn_blocking` extraction).
fn staging_dir_for(dest: &Path) -> PathBuf {
  let seq = STAGING_SEQ.fetch_add(1, Ordering::Relaxed);
  let pid = std::process::id();
  let mut staging = dest.as_os_str().to_owned();
  staging.push(format!(".tmp-{pid}-{seq}"));
  PathBuf::from(staging)
}

/// Best-effort removal of a staging (or stale destination) directory. A
/// failure is WARN-logged, never propagated: the caller already has (or is
/// producing) the real error/result to return, and a leftover `.tmp-*` dir
/// is a disk-hygiene issue, not a correctness one — nothing ever reads it,
/// since [`is_action_cached`] only ever looks at `dest`.
fn cleanup_staging(staging: &Path) {
  if let Err(e) = std::fs::remove_dir_all(staging)
    && e.kind() != std::io::ErrorKind::NotFound
  {
    tracing::warn!(staging = %staging.display(), error = %e, "staging dir cleanup failed");
  }
}

/// Promote a fully-extracted `staging` dir onto `dest` as the last,
/// atomic-on-same-filesystem step of an extraction.
///
/// Ordering matters here in a way the code itself can't show: `dest` may
/// still be a stale, non-watermarked leftover — either an old (pre-staging)
/// direct extraction that was interrupted, or a genuinely partial `dest`
/// left by a bug. It is cleared immediately before the rename so the window
/// in which `dest` is absent is as short as possible. If a concurrent
/// extraction is racing us for the same `dest`, one of two things happens:
/// either our `rename` lands first (the other extraction's later rename
/// then fails and it defers to us), or theirs already landed (our `rename`
/// fails with `dest` present, so we discard our own staging copy and defer
/// to them). Either way `dest` ends up holding exactly ONE complete tree —
/// never an interleaving of both, because extraction itself never writes
/// into `dest`.
///
/// # Errors
///
/// Returns `RunnerError::ActionDownload` when the rename fails and `dest`
/// still does not exist afterward (a genuine filesystem error, not a lost
/// race).
fn promote_staging(staging: &Path, dest: &Path) -> Result<(), RunnerError> {
  if dest.exists()
    && !is_action_cached(dest)
    && let Err(e) = std::fs::remove_dir_all(dest)
  {
    tracing::warn!(
      dest = %dest.display(),
      error = %e,
      "stale action cache dir removal failed before promote; rename may fail"
    );
  }

  match std::fs::rename(staging, dest) {
    Ok(()) => Ok(()),
    Err(rename_err) => {
      cleanup_staging(staging);
      if dest.exists() {
        // A concurrent extraction's rename already landed here first. Our
        // extraction was redundant, not wrong — `dest` holds a complete
        // tree either way. The caller's watermark write still runs
        // (idempotent) even if `dest` doesn't have one yet.
        Ok(())
      } else {
        Err(RunnerError::ActionDownload(format!(
          "rename {} -> {}: {rename_err}",
          staging.display(),
          dest.display()
        )))
      }
    },
  }
}

/// Fetch `tarball_url` and return a synchronous [`Read`] streaming its body,
/// suitable for [`extract_tarball`] inside `spawn_blocking`. Requires a
/// `User-Agent` — GitHub rejects plain `reqwest` requests without one.
///
/// # Errors
///
/// Returns `RunnerError::ActionDownload` on a request failure or a
/// non-success HTTP status.
async fn fetch_tarball_reader(
  client: &reqwest::Client,
  tarball_url: &str,
  token: Option<&str>,
) -> Result<impl Read + use<>, RunnerError> {
  let mut req = client
    .get(tarball_url)
    .header(reqwest::header::USER_AGENT, "toolu-runner")
    .header(reqwest::header::ACCEPT, "application/vnd.github+json");
  if let Some(t) = token {
    req = req.bearer_auth(t);
  }

  let response = req
    .send()
    .await
    .map_err(|e| RunnerError::ActionDownload(format!("fetch {tarball_url}: {e}")))?;

  let status = response.status();
  if !status.is_success() {
    let body = response.text().await.unwrap_or_default();
    return Err(RunnerError::ActionDownload(format!(
      "tarball {tarball_url} status {status}: {body}"
    )));
  }

  let stream = response
    .bytes_stream()
    .map(|chunk| chunk.map_err(std::io::Error::other));
  let stream_reader = tokio_util::io::StreamReader::new(stream);
  // Constructed here (an async context) so it captures the current Tokio
  // runtime handle, then moved into `spawn_blocking` — its documented use.
  Ok(tokio_util::io::SyncIoBridge::new(stream_reader))
}

/// Download an action tarball + extract to its cache directory.
/// No-op if cached (watermark present). Requires a `User-Agent` — GitHub rejects
/// plain `reqwest` requests without it.
///
/// Extraction lands in a unique sibling staging directory first and is
/// promoted onto `cache_dir` with a single [`std::fs::rename`] as the last
/// step before the watermark write (see [`promote_staging`]). This keeps
/// `cache_dir` always either absent or a complete tree, so an extraction
/// that outlives its job (e.g. an aborted background prefetch whose
/// `spawn_blocking` closure keeps running) can never leave the NEXT job's
/// extraction of the same action interleaved with this one's bytes.
///
/// # Errors
///
/// Returns `RunnerError::ActionDownload` on HTTP or extraction failure.
pub async fn download_and_extract_action(
  client: &reqwest::Client,
  tarball_url: &str,
  token: Option<&str>,
  cache_dir: &Path,
) -> Result<(), RunnerError> {
  if is_action_cached(cache_dir) {
    return Ok(());
  }

  let sync_reader = fetch_tarball_reader(client, tarball_url, token).await?;

  let staging = staging_dir_for(cache_dir);
  let staging_for_extract = staging.clone();
  let extract_result =
    tokio::task::spawn_blocking(move || extract_tarball(sync_reader, &staging_for_extract))
      .await
      .map_err(|e| RunnerError::ActionDownload(format!("extraction task failed: {e}")))
      .and_then(std::convert::identity);

  if let Err(err) = extract_result {
    cleanup_staging(&staging);
    return Err(err);
  }

  promote_staging(&staging, cache_dir)?;
  write_watermark(cache_dir)?;
  Ok(())
}

#[cfg(test)]
#[path = "tests/downloader.rs"]
mod tests;
