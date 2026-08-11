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

/// Watermark file path: `{cache_dir}/.completed`.
///
/// A CHILD of the cache dir rather than a sibling: [`download_and_extract_action`]
/// writes it into the staging dir BEFORE [`promote_staging`] renames that dir
/// onto `cache_dir`, so the rename publishes the extracted tree and its
/// completeness marker in one atomic step. A sibling watermark could only be
/// written after the rename, leaving a window in which `cache_dir` is a
/// complete-but-unwatermarked tree.
pub fn watermark_path(cache_dir: &Path) -> PathBuf {
  cache_dir.join(".completed")
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

/// Write the watermark file that marks an extracted tree as complete.
///
/// [`download_and_extract_action`] calls this on the STAGING dir, before
/// [`promote_staging`] renames it onto the cache dir — the rename is the
/// commit point, so a watermarked staging dir becomes a watermarked cache dir
/// atomically. Racing writers each watermark their OWN staging dir, so this
/// never contends.
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

/// Promote a fully-extracted, ALREADY-WATERMARKED `staging` dir onto `dest`
/// with a single rename — the commit point of an extraction.
///
/// Ordering matters here in a way the code itself can't show: the watermark
/// lives INSIDE `staging` ([`write_watermark`] runs before this call), so the
/// rename publishes the tree and its completeness marker together. `dest` is
/// therefore never observably a complete-but-unwatermarked tree, and this
/// function deliberately does NOT clear `dest` up front — such a pre-rename
/// cleanup is exactly what could delete a rival's just-committed tree in the
/// window before its watermark landed.
///
/// A failed rename means `dest` was already occupied (or the filesystem
/// refused the operation); which it is decides what happens next:
/// - [`is_action_cached`] holds: a concurrent extraction committed first. Our
///   copy was redundant, not wrong — discard the staging dir and defer to it.
/// - `dest` exists but is NOT watermarked: a stale partial (an interrupted
///   extraction from before this ordering, or an old direct-into-`dest` one).
///   Nothing valid can be reading it, so clear it and retry the rename ONCE.
/// - `dest` does not exist: a genuine filesystem error — propagate it.
///
/// Either way `dest` ends up holding exactly ONE complete tree — never an
/// interleaving of two, because extraction itself never writes into `dest`.
///
/// # Errors
///
/// Returns `RunnerError::ActionDownload` when the rename fails for a reason
/// other than a lost race — either with `dest` absent (a genuine filesystem
/// error) or with the single retry over a stale `dest` failing too.
fn promote_staging(staging: &Path, dest: &Path) -> Result<(), RunnerError> {
  let rename_err = match std::fs::rename(staging, dest) {
    Ok(()) => return Ok(()),
    Err(err) => err,
  };

  if is_action_cached(dest) {
    cleanup_staging(staging);
    return Ok(());
  }

  if dest.exists() {
    if let Err(remove_err) = std::fs::remove_dir_all(dest) {
      tracing::warn!(
        dest = %dest.display(),
        error = %remove_err,
        "stale action cache dir removal failed; retrying the promote rename anyway"
      );
    }
    match std::fs::rename(staging, dest) {
      Ok(()) => return Ok(()),
      Err(retry_err) => {
        cleanup_staging(staging);
        return Err(RunnerError::ActionDownload(format!(
          "rename {} -> {}: {rename_err}; retry after clearing a stale dest: {retry_err}",
          staging.display(),
          dest.display()
        )));
      },
    }
  }

  cleanup_staging(staging);
  Err(RunnerError::ActionDownload(format!(
    "rename {} -> {}: {rename_err}",
    staging.display(),
    dest.display()
  )))
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
    // A failed body read is reported, not swallowed: "status 500: <nothing>"
    // and "status 500: <could not be read>" are different diagnoses.
    let body = match response.text().await {
      Ok(body) => body,
      Err(e) => format!("(body read failed: {e})"),
    };
    return Err(RunnerError::ActionDownload(format!(
      "tarball {tarball_url} status {status}: {body}"
    )));
  }

  // The stream's `std::io::Error` surfaces from inside `extract_tarball`,
  // where nothing knows which URL the bytes came from — so name it here.
  let stream_url = tarball_url.to_owned();
  let stream = response.bytes_stream().map(move |chunk| {
    chunk.map_err(|e| std::io::Error::other(format!("tarball stream from {stream_url}: {e}")))
  });
  let stream_reader = tokio_util::io::StreamReader::new(stream);
  // Constructed here (an async context) so it captures the current Tokio
  // runtime handle, then moved into `spawn_blocking` — its documented use.
  Ok(tokio_util::io::SyncIoBridge::new(stream_reader))
}

/// Download an action tarball + extract to its cache directory.
/// No-op if cached (watermark present). Requires a `User-Agent` — GitHub rejects
/// plain `reqwest` requests without it.
///
/// Extraction lands in a unique sibling staging directory, the watermark is
/// written INTO that staging directory, and only then is it promoted onto
/// `cache_dir` with a single [`std::fs::rename`] (see [`promote_staging`]).
/// The rename is the commit point: `cache_dir` is always either absent or a
/// complete, watermarked tree — never a valid-but-unmarked one another
/// writer could mistake for a stale partial, and never an interleaving of
/// two extractions' bytes (an extraction that outlives its job — e.g. an
/// aborted background prefetch whose `spawn_blocking` closure keeps running —
/// still only ever writes into its own staging dir).
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
  // Best-effort fast path, not a lock: a concurrent extraction of the same
  // action may be in flight here, or start right after this check, in which
  // case both callers download and `promote_staging` resolves the race (one
  // tree is committed, the loser discards its staging copy). This check only
  // avoids the common redundant case — an action already committed on disk.
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

  // Watermark BEFORE the promote: the rename then commits tree + watermark
  // in one step (see `promote_staging`).
  if let Err(err) = write_watermark(&staging) {
    cleanup_staging(&staging);
    return Err(err);
  }

  promote_staging(&staging, cache_dir)
}

#[cfg(test)]
#[path = "tests/downloader.rs"]
mod tests;
