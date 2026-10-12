//! Node.js runtime resolution: pinned versions, cache layout, the offline
//! preseed directory, and [`ensure_node_runtime`] — the single entry point
//! every Node action stage uses.
//!
//! Order: a configured preseed dir (`TOOLU_RUNNER_NODE_DIR`) is authoritative
//! and never downloads; otherwise the per-target cache dir; otherwise a
//! download (nodejs.org for glibc/darwin, `actions/alpine_nodejs` for musl)
//! committed atomically after a startup check when the target is this host.

use std::path::{Path, PathBuf};

use shared::{RunnerConfig, RunnerError};

use super::download::{Install, install_runtime};
use super::platform::{Libc, NodeArch, NodeOs, NodeTarget, host_target};
use super::validate::validate_runtime;

/// Resolve a Node.js major version to the release upstream's runner pins
/// (actions/runner `externals.sh` @ v2.337.0).
///
/// Unknown majors fall back to node 20 with a warning.
pub fn node_version_for(major: u8) -> &'static str {
  match major {
    20 => "20.20.2",
    24 => "24.21.0",
    other => {
      tracing::warn!(major = other, "unknown node major, defaulting to node20");
      "20.20.2"
    },
  }
}

/// Cache directory of the host-OS nodejs.org build: `{data_dir}/node/{version}`.
pub fn node_cache_dir(data_dir: &Path, version: &str) -> PathBuf {
  data_dir.join("node").join(version)
}

/// Cache directory for `target`: `{data_dir}/node/{version}` (glibc/darwin)
/// or `{data_dir}/node/{version}-alpine` (musl).
pub fn node_runtime_dir(data_dir: &Path, version: &str, target: NodeTarget) -> PathBuf {
  data_dir.join("node").join(target.cache_name(version))
}

/// Path to node binary within cache directory.
pub fn node_binary_path(cache_dir: &Path) -> PathBuf {
  cache_dir.join("bin").join("node")
}

/// The preseeded binary for `major` on `target`, in upstream's `externals/`
/// layout: `<node_dir>/node24/bin/node`, `<node_dir>/node24_alpine/bin/node`.
pub fn preseed_binary_path(node_dir: &Path, major: u8, target: NodeTarget) -> PathBuf {
  node_binary_path(&node_dir.join(target.externals_name(major)))
}

/// Download hosts a runtime tarball URL is resolved against.
#[derive(Debug, Clone, Copy)]
pub struct DownloadBases<'a> {
  /// nodejs.org `dist` root (glibc Linux and darwin builds).
  pub nodejs: &'a str,
  /// `actions/alpine_nodejs` release-download root (musl x64 builds).
  pub alpine: &'a str,
}

/// The real hosts every production install uses.
pub const PRODUCTION_BASES: DownloadBases<'static> = DownloadBases {
  nodejs: "https://nodejs.org/dist",
  alpine: "https://github.com/actions/alpine_nodejs/releases/download",
};

/// Tarball URL for `version` on `target`, and how many leading path
/// components its entries carry before `bin/node`.
pub fn node_download_url(
  bases: &DownloadBases<'_>,
  version: &str,
  target: NodeTarget,
) -> (String, usize) {
  if target.libc == Libc::Musl {
    let base = bases.alpine;
    return (
      format!("{base}/v{version}/node-v{version}-alpine-x64.tar.gz"),
      0,
    );
  }
  let os = match target.os {
    NodeOs::Linux => "linux",
    NodeOs::Darwin => "darwin",
  };
  let arch = match target.arch {
    NodeArch::X64 => "x64",
    NodeArch::Arm64 => "arm64",
  };
  let base = bases.nodejs;
  (
    format!("{base}/v{version}/node-v{version}-{os}-{arch}.tar.gz"),
    1,
  )
}

/// Return a Node.js `major` binary that runs on `target`, acquiring it if
/// needed (see the module doc for the order).
///
/// # Errors
///
/// Returns `RunnerError::NodeRuntime` when a preseeded runtime is missing or
/// fails its startup check, or a download/extraction/startup check fails.
pub async fn ensure_node_runtime(
  client: &reqwest::Client,
  config: &RunnerConfig,
  major: u8,
  target: NodeTarget,
) -> Result<PathBuf, RunnerError> {
  let host_runnable = host_target().is_ok_and(|host| host == target);
  ensure_node_runtime_from(
    &PRODUCTION_BASES,
    client,
    config,
    major,
    target,
    host_runnable,
  )
  .await
}

/// [`ensure_node_runtime`] with injectable download hosts (tests serve
/// tarballs locally) and an explicit "runs on this host" decision, which
/// gates the startup check.
///
/// # Errors
///
/// See [`ensure_node_runtime`].
pub async fn ensure_node_runtime_from(
  bases: &DownloadBases<'_>,
  client: &reqwest::Client,
  config: &RunnerConfig,
  major: u8,
  target: NodeTarget,
  host_runnable: bool,
) -> Result<PathBuf, RunnerError> {
  if let Some(node_dir) = &config.node_dir {
    return preseeded(node_dir, major, target, host_runnable).await;
  }
  let version = node_version_for(major);
  let cache_dir = node_runtime_dir(&config.data_dir, version, target);
  let binary = node_binary_path(&cache_dir);
  if binary.exists() {
    return Ok(binary);
  }
  let (url, strip) = node_download_url(bases, version, target);
  tracing::info!(version, %target, url = %url, "downloading Node.js runtime");
  install_runtime(
    client,
    &Install {
      url: &url,
      strip,
      cache_dir: &cache_dir,
      validate_major: host_runnable.then_some(major),
    },
  )
  .await?;
  Ok(binary)
}

/// The authoritative preseed lookup: never downloads.
async fn preseeded(
  node_dir: &Path,
  major: u8,
  target: NodeTarget,
  host_runnable: bool,
) -> Result<PathBuf, RunnerError> {
  let binary = preseed_binary_path(node_dir, major, target);
  if !binary.exists() {
    return Err(RunnerError::NodeRuntime(format!(
      "Node.js runtime {} for {target} not found in TOOLU_RUNNER_NODE_DIR: expected {}",
      target.externals_name(major),
      binary.display()
    )));
  }
  if host_runnable {
    validate_runtime(&binary, major)
      .await
      .map_err(RunnerError::NodeRuntime)?;
  }
  Ok(binary)
}

#[cfg(test)]
#[path = "tests/runtime.rs"]
mod tests;
