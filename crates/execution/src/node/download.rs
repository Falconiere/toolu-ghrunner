//! Node.js runtime tarball download, extraction and atomic cache commit.
//!
//! Streams the body straight into a sync extractor on a blocking thread,
//! extracts into a unique sibling staging dir, optionally proves the runtime
//! starts there, and only then renames it onto the cache dir — so the cache
//! dir is always either absent or a complete, validated tree.
//!
//! A transient transport failure (connection error, 408/429/5xx, or the body
//! stream breaking mid-download) is retried up to [`DOWNLOAD_ATTEMPTS`]
//! times; anything else — a 4xx, a corrupt tarball, a failed start check —
//! fails on the first attempt.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use futures_util::StreamExt;
use reqwest::StatusCode;
use shared::RunnerError;

use super::runtime::node_binary_path;

/// Download attempts per install, so one dropped connection on a tens-of-MB
/// tarball does not fail the step.
const DOWNLOAD_ATTEMPTS: u32 = 3;

/// The pause before retry `n` is `n` times this.
const RETRY_DELAY: Duration = Duration::from_secs(1);

/// A failed download attempt, and whether a fresh request may succeed.
struct AttemptError {
  error: RunnerError,
  transient: bool,
}

impl AttemptError {
  fn permanent(error: RunnerError) -> Self {
    Self {
      error,
      transient: false,
    }
  }
}

/// Extract a tar.gz tarball into `dest`, dropping `.` components and then
/// the first `strip` components: nodejs.org tarballs nest everything under
/// `node-v{version}-{os}-{arch}/` (`strip = 1`), `actions/alpine_nodejs`
/// tarballs are rooted at `./` (`strip = 0`). An entry escaping `dest`
/// (`..` or an absolute path) is rejected.
///
/// Takes a plain [`Read`] rather than a buffered byte slice so the caller
/// can stream the tarball body straight off the network (via
/// `tokio_util::io::SyncIoBridge`) instead of holding the whole tarball in
/// memory. This is CPU-bound, synchronous work — every production call
/// site MUST run it inside `tokio::task::spawn_blocking`.
fn extract_tarball(reader: impl Read, dest: &Path, strip: usize) -> std::io::Result<()> {
  use flate2::read::GzDecoder;
  use std::path::Component;
  let decoder = GzDecoder::new(reader);
  let mut archive = tar::Archive::new(decoder);
  std::fs::create_dir_all(dest)?;
  for entry in archive.entries()? {
    let mut entry = entry?;
    let path = entry.path()?;
    let mut stripped = PathBuf::new();
    for component in path
      .components()
      .filter(|c| *c != Component::CurDir)
      .skip(strip)
    {
      match component {
        Component::Normal(part) => stripped.push(part),
        Component::Prefix(_) | Component::RootDir | Component::CurDir | Component::ParentDir => {
          return Err(std::io::Error::other(format!(
            "tarball entry {} escapes the runtime directory",
            path.display()
          )));
        },
      }
    }
    if stripped.as_os_str().is_empty() {
      continue;
    }
    let out = dest.join(stripped);
    if entry.header().entry_type().is_dir() {
      std::fs::create_dir_all(&out)?;
    } else {
      if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
      }
      let mut f = std::fs::File::create(&out)?;
      std::io::copy(&mut entry, &mut f)?;
      #[cfg(unix)]
      if let Ok(mode) = entry.header().mode() {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&out, std::fs::Permissions::from_mode(mode))?;
      }
    }
  }
  Ok(())
}

/// Fetch `url` and return a synchronous [`Read`] streaming its body,
/// suitable for [`extract_tarball`] inside `spawn_blocking`, plus a flag set
/// when that body stream fails — gzip/tar cannot tell a broken connection
/// from a corrupt tarball once the error reaches them.
///
/// # Errors
///
/// Returns a `RunnerError::NodeRuntime` on a request failure or a
/// non-success HTTP status, transient for a connection failure or a
/// 408/429/5xx.
async fn fetch_node_tarball_reader(
  client: &reqwest::Client,
  url: &str,
) -> Result<(impl Read + use<>, Arc<AtomicBool>), AttemptError> {
  let response = client.get(url).send().await.map_err(|e| AttemptError {
    transient: !e.is_builder(),
    error: RunnerError::NodeRuntime(format!("fetch {url}: {e}")),
  })?;

  let status = response.status();
  if !status.is_success() {
    // A failed body read is reported, not swallowed: "status 500: <nothing>"
    // and "status 500: <could not be read>" are different diagnoses.
    let body = match response.text().await {
      Ok(body) => body,
      Err(e) => format!("(body read failed: {e})"),
    };
    return Err(AttemptError {
      transient: status.is_server_error()
        || status == StatusCode::TOO_MANY_REQUESTS
        || status == StatusCode::REQUEST_TIMEOUT,
      error: RunnerError::NodeRuntime(format!("node tarball {url} status {status}: {body}")),
    });
  }

  // The stream's `std::io::Error` surfaces from inside `extract_tarball`,
  // where nothing knows which URL the bytes came from — so name it here.
  let stream_url = url.to_owned();
  let stream_failed = Arc::new(AtomicBool::new(false));
  let failed = Arc::clone(&stream_failed);
  let stream = response.bytes_stream().map(move |chunk| {
    chunk.map_err(|e| {
      failed.store(true, Ordering::Relaxed);
      std::io::Error::other(format!("tarball stream from {stream_url}: {e}"))
    })
  });
  let stream_reader = tokio_util::io::StreamReader::new(stream);
  // Constructed here (an async context) so it captures the current Tokio
  // runtime handle, then moved into `spawn_blocking` — its documented use.
  Ok((
    tokio_util::io::SyncIoBridge::new(stream_reader),
    stream_failed,
  ))
}

/// Monotonic counter disambiguating staging dirs created by this process.
/// Mirrors `execution::actions::downloader`'s scheme (PID + counter, no
/// UUID dependency).
static STAGING_SEQ: AtomicU64 = AtomicU64::new(0);

/// Build a unique sibling staging directory for `dest`:
/// `<dest>.tmp-<pid>-<seq>`. Extraction lands here first; [`promote_staging`]
/// renames it onto `dest` as the last step, so `dest` — the presence of
/// `node_binary_path(dest)` other jobs/processes check to skip a re-download —
/// never observes a partially-extracted tree, even under concurrent
/// cross-repo runner processes sharing the same node cache dir.
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
/// is a disk-hygiene issue, not a correctness one.
fn cleanup_staging(staging: &Path) {
  if let Err(e) = std::fs::remove_dir_all(staging)
    && e.kind() != std::io::ErrorKind::NotFound
  {
    tracing::warn!(
      staging = %staging.display(),
      error = %e,
      "node runtime staging dir cleanup failed"
    );
  }
}

/// Promote a fully-extracted `staging` dir onto `dest` with a single rename
/// — the commit point of a Node.js runtime install.
///
/// This install's completeness signal is the binary at
/// `node_binary_path(dest)`, which `extract_tarball` writes INSIDE the
/// staging dir (as `bin/node`), so the rename publishes the tree and its
/// completeness signal together — the mirror of `actions::downloader`'s
/// watermark-inside-staging ordering, without needing a separate marker
/// file. `dest` is therefore never observably a complete-but-unsignaled
/// tree, and this function deliberately does NOT clear `dest` up front: such
/// a pre-rename cleanup is exactly what could delete a rival's just-committed
/// tree.
///
/// A failed rename means `dest` was already occupied (or the filesystem
/// refused the operation); which it is decides what happens next:
/// - `node_binary_path(dest)` exists: a concurrent install (e.g. two runner
///   processes for different repos installing the same Node.js version)
///   committed first. Ours was redundant, not wrong — discard the staging
///   copy and defer to it.
/// - `dest` exists without the binary: a stale partial (an interrupted
///   pre-staging extraction, or one left by a bug). Nothing valid can be
///   reading it, so clear it and retry the rename ONCE.
/// - `dest` does not exist: a genuine filesystem error — propagate it.
///
/// Either way `dest` ends up holding exactly ONE complete tree — never an
/// interleaving of both, because extraction itself never writes into `dest`.
///
/// # Errors
///
/// Returns `RunnerError::NodeRuntime` when the rename fails for a reason
/// other than a lost race — either with `dest` absent (a genuine filesystem
/// error) or with the single retry over a stale `dest` failing too.
fn promote_staging(staging: &Path, dest: &Path) -> Result<(), RunnerError> {
  let rename_err = match std::fs::rename(staging, dest) {
    Ok(()) => return Ok(()),
    Err(err) => err,
  };

  if node_binary_path(dest).exists() {
    cleanup_staging(staging);
    return Ok(());
  }

  if dest.exists() {
    // Re-check completeness at the last possible moment: a rival's rename can
    // land between the check above and this removal, and deleting its
    // just-committed runtime would strand later steps. Now-complete => lost
    // race, defer to the winner.
    if node_binary_path(dest).exists() {
      cleanup_staging(staging);
      return Ok(());
    }
    if let Err(remove_err) = std::fs::remove_dir_all(dest) {
      tracing::warn!(
        dest = %dest.display(),
        error = %remove_err,
        "stale node cache dir removal failed; retrying the promote rename anyway"
      );
    }
    match std::fs::rename(staging, dest) {
      Ok(()) => return Ok(()),
      Err(retry_err) => {
        cleanup_staging(staging);
        return Err(RunnerError::NodeRuntime(format!(
          "rename {} -> {}: {rename_err}; retry after clearing a stale dest: {retry_err}",
          staging.display(),
          dest.display()
        )));
      },
    }
  }

  cleanup_staging(staging);
  Err(RunnerError::NodeRuntime(format!(
    "rename {} -> {}: {rename_err}",
    staging.display(),
    dest.display()
  )))
}

/// Where one runtime install comes from and how it is proven.
pub(super) struct Install<'a> {
  /// Tarball URL.
  pub url: &'a str,
  /// Leading components to strip (see [`extract_tarball`]).
  pub strip: usize,
  /// Cache dir the install commits to.
  pub cache_dir: &'a Path,
  /// `Some(major)` when the runtime must start on this host before commit.
  pub validate_major: Option<u8>,
}

/// One download attempt: fetch `install.url` and extract it into `staging`.
async fn download_into(
  client: &reqwest::Client,
  install: &Install<'_>,
  staging: &Path,
) -> Result<(), AttemptError> {
  let (sync_reader, stream_failed) = fetch_node_tarball_reader(client, install.url).await?;
  let staging_for_extract = staging.to_path_buf();
  let strip = install.strip;
  tokio::task::spawn_blocking(move || extract_tarball(sync_reader, &staging_for_extract, strip))
    .await
    .map_err(|e| {
      AttemptError::permanent(RunnerError::NodeRuntime(format!(
        "extraction task failed: {e}"
      )))
    })?
    .map_err(|e| AttemptError {
      error: RunnerError::NodeRuntime(format!("extract node tarball: {e}")),
      transient: stream_failed.load(Ordering::Relaxed),
    })
}

/// Download, extract and (optionally) start-check a runtime in staging,
/// then commit it onto `install.cache_dir`. Transient transport failures
/// are retried (see the module doc).
///
/// # Errors
///
/// Returns `RunnerError::NodeRuntime` on HTTP, extraction, validation or
/// commit failures; the staging dir is removed and the cache dir untouched.
pub(super) async fn install_runtime(
  client: &reqwest::Client,
  install: &Install<'_>,
) -> Result<(), RunnerError> {
  let mut attempt = 1;
  let staging = loop {
    let staging = staging_dir_for(install.cache_dir);
    let Err(failed) = download_into(client, install, &staging).await else {
      break staging;
    };
    cleanup_staging(&staging);
    if !failed.transient || attempt >= DOWNLOAD_ATTEMPTS {
      return Err(failed.error);
    }
    tracing::warn!(
      url = install.url,
      attempt,
      error = %failed.error,
      "node runtime download failed; retrying"
    );
    tokio::time::sleep(RETRY_DELAY * attempt).await;
    attempt += 1;
  };
  if let Some(major) = install.validate_major
    && let Err(e) = super::validate::validate_runtime(&node_binary_path(&staging), major).await
  {
    cleanup_staging(&staging);
    return Err(RunnerError::NodeRuntime(format!(
      "{} from {} was not installed: {e}",
      install.cache_dir.display(),
      install.url
    )));
  }
  promote_staging(&staging, install.cache_dir)
}

#[cfg(test)]
#[path = "tests/download.rs"]
mod tests;
