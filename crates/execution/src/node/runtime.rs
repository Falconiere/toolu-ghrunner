//! Node.js runtime detection and caching.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use futures_util::StreamExt;
use shared::RunnerError;

/// Resolve a Node.js major version to a concrete release version string.
///
/// Unknown majors fall back to node 20 LTS with a warning.
pub fn node_version_for(major: u8) -> &'static str {
  match major {
    20 => "20.18.3",
    24 => "24.0.2",
    other => {
      tracing::warn!(major = other, "unknown node major, defaulting to node20");
      "20.18.3"
    },
  }
}

/// Cache directory: `{data_dir}/node/{version}`.
pub fn node_cache_dir(data_dir: &Path, version: &str) -> PathBuf {
  data_dir.join("node").join(version)
}

/// Path to node binary within cache directory.
pub fn node_binary_path(cache_dir: &Path) -> PathBuf {
  cache_dir.join("bin").join("node")
}

/// Download URL for a Node.js release tarball, resolved against `base_url`
/// (production always resolves against [`NODE_DOWNLOAD_BASE`] via
/// [`ensure_node_runtime`]; tests can point this at a local HTTP server).
pub fn node_download_url(base_url: &str, version: &str, os: &str, arch: &str) -> String {
  format!("{base_url}/v{version}/node-v{version}-{os}-{arch}.tar.gz")
}

/// Detect current platform for Node.js downloads.
///
/// Returns `("linux"|"darwin", "x64"|"arm64")`.
pub fn detect_platform() -> (&'static str, &'static str) {
  let os = match std::env::consts::OS {
    "macos" => "darwin",
    _ => "linux",
  };

  let arch = match std::env::consts::ARCH {
    "aarch64" => "arm64",
    _ => "x64",
  };

  (os, arch)
}

/// Extract a tar.gz tarball into a directory, stripping the leading
/// top-level component (e.g. `node-v20.18.3-linux-x64/`).
///
/// Local copy so the `node` module compiles in step 4b before the real
/// `execution::actions::downloader` lands in step 4c. The two share the
/// same stripping semantics.
///
/// Takes a plain [`Read`] rather than a buffered byte slice so the caller
/// can stream the tarball body straight off the network (via
/// `tokio_util::io::SyncIoBridge`) instead of holding the whole tarball in
/// memory. This is CPU-bound, synchronous work — every production call
/// site MUST run it inside `tokio::task::spawn_blocking`.
fn extract_tarball(reader: impl Read, dest: &Path) -> std::io::Result<()> {
  use flate2::read::GzDecoder;
  let decoder = GzDecoder::new(reader);
  let mut archive = tar::Archive::new(decoder);
  std::fs::create_dir_all(dest)?;
  for entry in archive.entries()? {
    let mut entry = entry?;
    let path = entry.path()?;
    let stripped = path.components().skip(1).collect::<std::path::PathBuf>();
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
/// suitable for [`extract_tarball`] inside `spawn_blocking`.
///
/// # Errors
///
/// Returns `RunnerError::NodeRuntime` on a request failure or a non-success
/// HTTP status.
async fn fetch_node_tarball_reader(
  client: &reqwest::Client,
  url: &str,
) -> Result<impl Read + use<>, RunnerError> {
  let response = client
    .get(url)
    .send()
    .await
    .map_err(|e| RunnerError::NodeRuntime(format!("fetch {url}: {e}")))?;

  let status = response.status();
  if !status.is_success() {
    let body = response.text().await.unwrap_or_default();
    return Err(RunnerError::NodeRuntime(format!(
      "node tarball {url} status {status}: {body}"
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

/// Monotonic counter disambiguating staging dirs created by this process.
/// Mirrors `execution::actions::downloader`'s scheme (PID + counter, no
/// UUID dependency) — this module is a deliberate local copy (see the
/// module doc on [`extract_tarball`]), so the staging helpers are too.
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

/// Promote a fully-extracted `staging` dir onto `dest` as the last,
/// atomic-on-same-filesystem step of a Node.js runtime install.
///
/// Ordering matters here in a way the code itself can't show: `dest` may
/// still be a stale, incomplete leftover — either an old (pre-staging)
/// direct extraction that was interrupted, or a genuinely partial `dest`
/// left by a bug — signaled by the binary not being present at
/// `node_binary_path(dest)`. It is cleared immediately before the rename so
/// the window in which `dest` is absent is as short as possible. If a
/// concurrent extraction (e.g. two runner processes for different repos
/// both installing the same Node.js version) is racing us for the same
/// `dest`, one of two things happens: either our `rename` lands first (the
/// other extraction's later rename then fails and it defers to us), or
/// theirs already landed (our `rename` fails with `dest` present, so we
/// discard our own staging copy and defer to them). Either way `dest` ends
/// up holding exactly ONE complete tree — never an interleaving of both,
/// because extraction itself never writes into `dest`.
///
/// # Errors
///
/// Returns `RunnerError::NodeRuntime` when the rename fails and `dest`
/// still does not exist afterward (a genuine filesystem error, not a lost
/// race).
fn promote_staging(staging: &Path, dest: &Path) -> Result<(), RunnerError> {
  if dest.exists()
    && !node_binary_path(dest).exists()
    && let Err(e) = std::fs::remove_dir_all(dest)
  {
    tracing::warn!(
      dest = %dest.display(),
      error = %e,
      "stale node cache dir removal failed before promote; rename may fail"
    );
  }

  match std::fs::rename(staging, dest) {
    Ok(()) => Ok(()),
    Err(rename_err) => {
      cleanup_staging(staging);
      if dest.exists() {
        // A concurrent install's rename already landed here first. Ours was
        // redundant, not wrong — `dest` holds a complete tree either way.
        Ok(())
      } else {
        Err(RunnerError::NodeRuntime(format!(
          "rename {} -> {}: {rename_err}",
          staging.display(),
          dest.display()
        )))
      }
    },
  }
}

/// The real Node.js download host every production install resolves
/// tarball URLs against. [`ensure_node_runtime_from`] takes the base as a
/// parameter (this constant is the only value the public
/// [`ensure_node_runtime`] ever calls it with) so an end-to-end test can
/// drive the SAME fetch → stage → extract → promote sequence against a
/// local HTTP server instead of `nodejs.org`.
const NODE_DOWNLOAD_BASE: &str = "https://nodejs.org/dist";

/// Download and cache Node.js if not already present, returning the binary path.
///
/// Thin wrapper around [`ensure_node_runtime_from`] pinned to the real
/// [`NODE_DOWNLOAD_BASE`].
///
/// # Errors
///
/// Returns `RunnerError::NodeRuntime` on HTTP or extraction failures.
pub async fn ensure_node_runtime(
  client: &reqwest::Client,
  data_dir: &Path,
  major: u8,
) -> Result<PathBuf, RunnerError> {
  ensure_node_runtime_from(NODE_DOWNLOAD_BASE, client, data_dir, major).await
}

/// Download and cache Node.js if not already present, returning the binary
/// path. `base_url` is a parameter (rather than hardcoded) so this exact
/// function — the full fetch → stage → extract → promote sequence
/// [`ensure_node_runtime`] runs in production — can be driven end-to-end
/// against a local HTTP server in tests; [`ensure_node_runtime`] always
/// calls it with [`NODE_DOWNLOAD_BASE`].
///
/// Skips download when the binary already exists on disk. Otherwise fetches
/// the tarball and extracts it with first-component stripping (the
/// tarball's `node-v{version}-{os}-{arch}/` prefix is removed) into a
/// unique sibling staging directory, then promotes it onto the cache dir
/// with a single [`std::fs::rename`] (see [`promote_staging`]). This keeps
/// the cache dir always either absent or a complete tree, so an extraction
/// from one runner process can never interleave with another's for the same
/// Node.js version.
///
/// # Errors
///
/// Returns `RunnerError::NodeRuntime` on HTTP or extraction failures.
async fn ensure_node_runtime_from(
  base_url: &str,
  client: &reqwest::Client,
  data_dir: &Path,
  major: u8,
) -> Result<PathBuf, RunnerError> {
  let version = node_version_for(major);
  let cache_dir = node_cache_dir(data_dir, version);
  let binary = node_binary_path(&cache_dir);

  if binary.exists() {
    return Ok(binary);
  }

  let (os, arch) = detect_platform();
  let url = node_download_url(base_url, version, os, arch);

  tracing::info!(version, url = %url, "downloading Node.js runtime");

  let sync_reader = fetch_node_tarball_reader(client, &url).await?;

  let staging = staging_dir_for(&cache_dir);
  let staging_for_extract = staging.clone();
  let extract_result =
    tokio::task::spawn_blocking(move || extract_tarball(sync_reader, &staging_for_extract))
      .await
      .map_err(|e| RunnerError::NodeRuntime(format!("extraction task failed: {e}")))
      .and_then(|inner| {
        inner.map_err(|e| RunnerError::NodeRuntime(format!("extract node tarball: {e}")))
      });

  if let Err(err) = extract_result {
    cleanup_staging(&staging);
    return Err(err);
  }

  promote_staging(&staging, &cache_dir)?;

  Ok(binary)
}

#[cfg(test)]
#[path = "tests/runtime.rs"]
mod tests;
