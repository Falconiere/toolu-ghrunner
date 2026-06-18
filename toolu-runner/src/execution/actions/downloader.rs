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
    // Tar-slip guard: a malicious or compromised action can include
    // `../` components in entry paths. Joining those under `dest` would
    // escape the cache directory (and any file the runner user can write).
    // Reject any entry whose stripped path contains a parent-dir, root,
    // or (Windows) drive prefix component before touching the filesystem.
    if stripped.components().any(|c| {
      matches!(
        c,
        std::path::Component::ParentDir
          | std::path::Component::RootDir
          | std::path::Component::Prefix(_)
      )
    }) {
      return Err(RunnerError::ActionDownload(format!(
        "tar slip: entry {path:?} escapes dest"
      )));
    }
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

#[cfg(test)]
mod tests {
  use super::*;
  use flate2::Compression;
  use flate2::write::GzEncoder;
  use std::env;
  use std::io::Write;
  use tar::{Builder, Header};

  fn tmp_dest(label: &str) -> PathBuf {
    let mut p = env::temp_dir();
    p.push(format!(
      "toolu-tarslip-test-{label}-{}",
      uuid::Uuid::new_v4()
    ));
    p
  }

  /// Build a tar.gz in memory containing a single entry with the given
  /// tar-path (the path stored in the tar header, before prefix-strip).
  /// Uses the safe `tar::Builder` API; paths with `..` are not
  /// constructible via this helper — for those, use `build_tarball_raw`.
  fn build_tarball(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut raw = Vec::new();
    {
      let mut builder = Builder::new(&mut raw);
      for (path, body) in entries {
        let mut header = Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
          .append_data(&mut header, path, *body)
          .expect("append entry");
      }
      builder.finish().expect("finish tar");
    }
    let mut gz = Vec::new();
    {
      let mut enc = GzEncoder::new(&mut gz, Compression::default());
      enc.write_all(&raw).expect("gzip write");
      enc.finish().expect("gzip finish");
    }
    gz
  }

  /// Construct a single-entry tar.gz tarball with a raw path string, so we
  /// can include `..` and absolute-prefix paths that the safe
  /// `tar::Builder` API rejects. The header is a USTAR-format 512-byte
  /// block with a manually-computed checksum, followed by the entry data
  /// padded to a multiple of 512, followed by two zero blocks.
  fn build_tarball_raw(path: &str, body: &[u8]) -> Vec<u8> {
    let mut header = [0_u8; 512];
    // Path: first 100 bytes, NUL-padded.
    let path_bytes = path.as_bytes();
    assert!(path_bytes.len() < 100, "test path must fit in 100 bytes");
    header
      .get_mut(..path_bytes.len())
      .expect("path length asserted < 100")
      .copy_from_slice(path_bytes);
    // Mode (octal, NUL-terminated, 7 chars + NUL).
    header[100..107].copy_from_slice(b"0000644");
    header[107] = 0;
    // uid, gid (octal, NUL-terminated, 7 chars + NUL).
    header[108..115].copy_from_slice(b"0000000");
    header[115] = 0;
    header[116..123].copy_from_slice(b"0000000");
    header[123] = 0;
    // Size (octal, 11 chars + NUL).
    let size_str = format!("{:011o}\0", body.len());
    header[124..136].copy_from_slice(size_str.as_bytes());
    // mtime (octal, 11 chars + NUL).
    header[135] = 0; // overwritten by the next line
    let mtime_str = format!("{:011o}\0", 0_u64);
    header[135..147].copy_from_slice(mtime_str.as_bytes());
    // Wait — size is 124..135 (12 bytes) and mtime starts at 135. The
    // spec says size is 124..136 (12 bytes). Let me re-check the
    // header layout: mode 100..108 (8), uid 108..116 (8), gid 116..124
    // (8), size 124..136 (12), mtime 136..148 (12), checksum
    // 148..156 (8), typeflag 156 (1), linkname 157..257 (100), magic
    // 257..265 (8), version 265..273 (8). Total = 273. The remaining
    // bytes are zeroed. Let me redo:
    let mut header = [0_u8; 512];
    // Path
    let path_bytes = path.as_bytes();
    assert!(path_bytes.len() < 100);
    header
      .get_mut(..path_bytes.len())
      .expect("path length asserted < 100")
      .copy_from_slice(path_bytes);
    // Mode
    header[100..107].copy_from_slice(b"0000644");
    header[107] = 0;
    // uid
    header[108..115].copy_from_slice(b"0000000");
    header[115] = 0;
    // gid
    header[116..123].copy_from_slice(b"0000000");
    header[123] = 0;
    // Size (12 bytes at 124..136)
    let size_str = format!("{:011o}\0", body.len());
    header[124..136].copy_from_slice(size_str.as_bytes());
    // mtime (12 bytes at 136..148)
    let mtime_str = format!("{:011o}\0", 0_u64);
    header[136..148].copy_from_slice(mtime_str.as_bytes());
    // Checksum placeholder: 8 spaces (148..156)
    header[148..156].copy_from_slice(b"        ");
    // Typeflag: '0' = regular file (156)
    header[156] = b'0';
    // Magic: "ustar\0" (257..263), then "00" (version at 263..265).
    // The PAX/USTAR layout actually has magic at 257..265 (8 bytes)
    // and version at 265..273. Use "ustar  \0" for POSIX.
    header[257..265].copy_from_slice(b"ustar  \0");
    header[265..267].copy_from_slice(b"00");

    // Compute checksum: sum of all bytes in the header (treating the
    // 8-byte checksum field as 8 spaces, which is what we just wrote).
    let checksum: u32 = header.iter().map(|b| *b as u32).sum();
    let chk_str = format!("{:06o}\0 ", checksum);
    header[148..156].copy_from_slice(chk_str.as_bytes());

    // Build the body, padded to 512.
    let mut entry = Vec::new();
    entry.extend_from_slice(&header);
    entry.extend_from_slice(body);
    let pad = (512 - (body.len() % 512)) % 512;
    entry.resize(entry.len() + pad, 0);
    // Two 512-byte zero blocks = end-of-archive marker.
    entry.resize(entry.len() + 1024, 0);

    // Gzip-wrap.
    let mut gz = Vec::new();
    {
      let mut enc = GzEncoder::new(&mut gz, Compression::default());
      enc.write_all(&entry).expect("gzip write");
      enc.finish().expect("gzip finish");
    }
    gz
  }

  #[test]
  fn normal_prefix_is_stripped_and_extracted() {
    let dest = tmp_dest("normal");
    let tar = build_tarball(&[("actions-checkout-v4-abc123/README.md", b"hello")]);
    extract_tarball(&tar, &dest).expect("normal tarball must extract");
    let read = std::fs::read(dest.join("README.md")).expect("file present");
    assert_eq!(read, b"hello");
  }

  #[test]
  fn parent_dir_entry_is_rejected() {
    let dest = tmp_dest("slip");
    // A malicious tarball that, after the GitHub prefix strip, still has
    // a `..` component. Without the tar-slip guard, joining this under
    // `dest` would write outside the dest directory.
    let tar = build_tarball_raw("evil-action-v1-abc123/../../../tmp/pwn", b"bad");
    let result = extract_tarball(&tar, &dest);
    assert!(matches!(result, Err(RunnerError::ActionDownload(_))));
    // Nothing should have been written into dest.
    assert_eq!(std::fs::read_dir(&dest).map(|i| i.count()).unwrap_or(0), 0);
  }

  #[test]
  fn mid_path_parent_dir_is_rejected() {
    let dest = tmp_dest("mid");
    // Even a single `..` mid-path (not just at the root) is a slip.
    let tar = build_tarball_raw("evil-action-v1-abc123/dir/../../escape", b"bad");
    let result = extract_tarball(&tar, &dest);
    assert!(matches!(result, Err(RunnerError::ActionDownload(_))));
  }

  #[test]
  fn good_entry_alongside_slip_is_not_extracted() {
    // A tarball with a slip entry: extraction must fail, and the good
    // entry must NOT have been written.
    let dest = tmp_dest("mixed");
    let slip_tar = build_tarball_raw("evil-action-v1-abc123/../escape", b"bad");
    let good_tar = build_tarball(&[("good-action-v1-abc/README.md", b"good")]);
    // Build a combined tarball by concatenating the slip + good tar
    // (after the slip has already been written). The tar format is
    // a stream; concatenating two valid tarballs yields a single valid
    // multi-entry tarball.
    let mut slip_gz = std::io::Cursor::new(slip_tar);
    let mut decoder = flate2::read::GzDecoder::new(&mut slip_gz);
    let mut slip_raw = Vec::new();
    use std::io::Read;
    decoder.read_to_end(&mut slip_raw).expect("decode slip");
    let mut good_gz = std::io::Cursor::new(good_tar);
    let mut decoder = flate2::read::GzDecoder::new(&mut good_gz);
    let mut good_raw = Vec::new();
    decoder.read_to_end(&mut good_raw).expect("decode good");
    let mut combined = slip_raw;
    combined.extend(good_raw);
    let mut combined_gz = Vec::new();
    {
      let mut enc = GzEncoder::new(&mut combined_gz, Compression::default());
      enc.write_all(&combined).expect("re-gzip");
      enc.finish().expect("gzip finish");
    }
    let result = extract_tarball(&combined_gz, &dest);
    assert!(matches!(result, Err(RunnerError::ActionDownload(_))));
    assert!(!dest.join("README.md").exists());
  }
}
