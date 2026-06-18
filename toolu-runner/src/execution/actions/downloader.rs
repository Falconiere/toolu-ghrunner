use std::io::Read;
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;
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
/// # Errors
///
/// Returns `RunnerError::ActionDownload` on extraction failures.
pub fn extract_tarball(tarball_bytes: &[u8], dest: &Path) -> Result<(), RunnerError> {
  std::fs::create_dir_all(dest)
    .map_err(|e| RunnerError::ActionDownload(format!("mkdir {}: {e}", dest.display())))?;

  let decoder = GzDecoder::new(tarball_bytes);
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
    let target = dest.join(&stripped);

    if let Some(parent) = target.parent() {
      std::fs::create_dir_all(parent)
        .map_err(|e| RunnerError::ActionDownload(format!("mkdir: {e}")))?;
    }

    if entry.header().entry_type().is_dir() {
      std::fs::create_dir_all(&target)
        .map_err(|e| RunnerError::ActionDownload(format!("mkdir: {e}")))?;
    } else {
      let mut content = Vec::new();
      entry
        .read_to_end(&mut content)
        .map_err(|e| RunnerError::ActionDownload(format!("read entry: {e}")))?;
      std::fs::write(&target, &content)
        .map_err(|e| RunnerError::ActionDownload(format!("write: {e}")))?;

      #[cfg(unix)]
      set_executable_if_needed(&target, &entry);
    }
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
/// # Errors
///
/// Returns `RunnerError::ActionDownload` on filesystem failure.
pub fn write_watermark(cache_dir: &Path) -> Result<(), RunnerError> {
  let wm = watermark_path(cache_dir);
  std::fs::write(&wm, b"")
    .map_err(|e| RunnerError::ActionDownload(format!("watermark {}: {e}", wm.display())))
}

/// Download an action tarball + extract to its cache directory.
/// No-op if cached (watermark present). Requires a `User-Agent` — GitHub rejects
/// plain `reqwest` requests without it.
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

  let bytes = response
    .bytes()
    .await
    .map_err(|e| RunnerError::ActionDownload(format!("read tarball body: {e}")))?;

  extract_tarball(&bytes, cache_dir)?;
  write_watermark(cache_dir)?;
  Ok(())
}
