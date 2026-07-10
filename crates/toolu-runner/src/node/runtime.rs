//! Node.js runtime detection and caching.

use std::path::{Path, PathBuf};

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

/// Download URL for a Node.js release tarball.
pub fn node_download_url(version: &str, os: &str, arch: &str) -> String {
  format!("https://nodejs.org/dist/v{version}/node-v{version}-{os}-{arch}.tar.gz")
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
fn extract_tarball(bytes: &[u8], dest: &Path) -> std::io::Result<()> {
  use flate2::read::GzDecoder;
  let decoder = GzDecoder::new(bytes);
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

/// Download and cache Node.js if not already present, returning the binary path.
///
/// Skips download when the binary already exists on disk. Otherwise fetches
/// the tarball from `nodejs.org` and extracts it with first-component stripping
/// (the tarball's `node-v{version}-{os}-{arch}/` prefix is removed).
///
/// # Errors
///
/// Returns `RunnerError::NodeRuntime` on HTTP or extraction failures.
pub async fn ensure_node_runtime(
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
  let url = node_download_url(version, os, arch);

  tracing::info!(version, url = %url, "downloading Node.js runtime");

  let response = client
    .get(&url)
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

  let bytes = response
    .bytes()
    .await
    .map_err(|e| RunnerError::NodeRuntime(format!("read tarball body: {e}")))?;

  extract_tarball(&bytes, &cache_dir)
    .map_err(|e| RunnerError::NodeRuntime(format!("extract node tarball: {e}")))?;

  Ok(binary)
}
