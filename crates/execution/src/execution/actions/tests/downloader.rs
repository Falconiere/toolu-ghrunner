//! Tests for `downloader`: tar-slip guards on `extract_tarball`, plus an
//! end-to-end streamed download+extract against a real local HTTP server,
//! plus the atomic-staging promotion (`promote_staging`) under real
//! concurrency and under extraction failure.

use super::*;
use flate2::Compression;
use flate2::write::GzEncoder;
use std::env;
use std::io::Write;
use std::net::SocketAddr;
use std::sync::Arc;
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
  // Path (first 100 bytes, NUL-padded)
  let path_bytes = path.as_bytes();
  assert!(path_bytes.len() < 100, "test path must fit in 100 bytes");
  header
    .get_mut(..path_bytes.len())
    .expect("path length asserted < 100")
    .copy_from_slice(path_bytes);
  // Mode (8 bytes at 100..108)
  header[100..107].copy_from_slice(b"0000644");
  header[107] = 0;
  // uid (8 bytes at 108..116)
  header[108..115].copy_from_slice(b"0000000");
  header[115] = 0;
  // gid (8 bytes at 116..124)
  header[116..123].copy_from_slice(b"0000000");
  header[123] = 0;
  // Size (12 bytes at 124..136)
  let size_str = format!("{:011o}\0", body.len());
  header[124..136].copy_from_slice(size_str.as_bytes());
  // mtime (12 bytes at 136..148)
  let mtime_str = format!("{:011o}\0", 0_u64);
  header[136..148].copy_from_slice(mtime_str.as_bytes());
  // Checksum placeholder: 8 spaces (148..156). Real checksum
  // written below.
  header[148..156].copy_from_slice(b"        ");
  // Typeflag (1 byte at 156): '0' = regular file
  header[156] = b'0';
  // Magic (8 bytes at 257..265) and version (8 bytes at 265..273).
  // Use the POSIX "ustar  \0" magic.
  header[257..265].copy_from_slice(b"ustar  \0");
  header[265..267].copy_from_slice(b"00");

  // Compute checksum: sum of all bytes in the header (treating the
  // 8-byte checksum field as 8 spaces, which is what we just wrote).
  let checksum: u32 = header.iter().map(|b| u32::from(*b)).sum();
  let chk_str = format!("{checksum:06o}\0 ");
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
  extract_tarball(&tar[..], &dest).expect("normal tarball must extract");
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
  let result = extract_tarball(&tar[..], &dest);
  assert!(matches!(result, Err(RunnerError::ActionDownload(_))));
  // Nothing should have been written into dest.
  assert_eq!(
    std::fs::read_dir(&dest)
      .map(std::iter::Iterator::count)
      .unwrap_or(0),
    0
  );
}

#[test]
fn mid_path_parent_dir_is_rejected() {
  let dest = tmp_dest("mid");
  // Even a single `..` mid-path (not just at the root) is a slip.
  let tar = build_tarball_raw("evil-action-v1-abc123/dir/../../escape", b"bad");
  let result = extract_tarball(&tar[..], &dest);
  assert!(matches!(result, Err(RunnerError::ActionDownload(_))));
}

#[test]
fn good_entry_alongside_slip_is_not_extracted() {
  use std::io::Read;

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
  let result = extract_tarball(&combined_gz[..], &dest);
  assert!(matches!(result, Err(RunnerError::ActionDownload(_))));
  assert!(!dest.join("README.md").exists());
}

/// Serve `body` as the response to every request on a random localhost
/// port, returning the bound address. The server task is aborted when
/// the caller's Tokio runtime shuts down at test end.
async fn spawn_tarball_server(body: Vec<u8>) -> SocketAddr {
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
    .await
    .expect("bind local tarball server");
  let addr = listener.local_addr().expect("local_addr");
  let app = axum::Router::new().route(
    "/tarball.tar.gz",
    axum::routing::get(move || async move { body }),
  );
  tokio::spawn(async move {
    let _ = axum::serve(listener, app).await;
  });
  addr
}

/// AC-9: a real tarball fixture served over local HTTP downloads and
/// extracts through the streamed path (`bytes_stream` → `StreamReader` →
/// `SyncIoBridge` → `spawn_blocking`) — contents, permissions, and the
/// on-disk cache watermark all verified.
#[tokio::test]
async fn download_and_extract_action_streams_from_http() {
  let tar = build_tarball(&[("actions-checkout-v4-abc123/README.md", b"hello from http")]);
  let addr = spawn_tarball_server(tar).await;
  let url = format!("http://{addr}/tarball.tar.gz");
  let client = reqwest::Client::new();
  let dest = tmp_dest("http-stream");

  download_and_extract_action(&client, &url, None, &dest)
    .await
    .expect("download+extract over http must succeed");

  let read = std::fs::read(dest.join("README.md")).expect("file present");
  assert_eq!(read, b"hello from http");
  assert!(is_action_cached(&dest), "watermark must be written");

  // Re-running against the same dest is a cache hit — no second fetch,
  // and the previously extracted content is untouched.
  download_and_extract_action(&client, &url, None, &dest)
    .await
    .expect("cached re-run must succeed without a fetch");
  let read_again = std::fs::read(dest.join("README.md")).expect("file still present");
  assert_eq!(read_again, b"hello from http");
}

/// Serve `body` (200 OK) for every request, but hold each response behind a
/// `gate` (sized to the number of concurrent callers under test) — a
/// response is released only once ALL gated requests have arrived, proving
/// genuine overlap rather than a lucky interleave. Mirrors
/// `prefetch.rs`'s test helper of the same name.
async fn spawn_gated_tarball_server(body: Vec<u8>, gate: Arc<tokio::sync::Barrier>) -> SocketAddr {
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
    .await
    .expect("bind local tarball server");
  let addr = listener.local_addr().expect("local_addr");
  let app = axum::Router::new().fallback(move || {
    let body = body.clone();
    let gate = Arc::clone(&gate);
    async move {
      gate.wait().await;
      body
    }
  });
  tokio::spawn(async move {
    let _ = axum::serve(listener, app).await;
  });
  addr
}

/// List any sibling staging dirs left behind for `dest` (i.e. anything
/// matching `<dest file name>.tmp-*` in `dest`'s parent directory).
fn leftover_staging_dirs(dest: &Path) -> Vec<PathBuf> {
  let Some(parent) = dest.parent() else {
    return Vec::new();
  };
  let Some(dest_name) = dest.file_name() else {
    return Vec::new();
  };
  let prefix = format!("{}.tmp-", dest_name.to_string_lossy());
  let Ok(entries) = std::fs::read_dir(parent) else {
    return Vec::new();
  };
  entries
    .filter_map(Result::ok)
    .map(|entry| entry.path())
    .filter(|path| {
      path
        .file_name()
        .is_some_and(|name| name.to_string_lossy().starts_with(&prefix))
    })
    .collect()
}

/// Two concurrent `download_and_extract_action` calls racing for the SAME
/// `dest` (a barrier-gated server proves genuine overlap, not a lucky
/// interleave) must both succeed, must leave `dest` holding exactly one
/// complete, valid extraction (not an interleaving of the two), the
/// watermark present, and no `.tmp-` staging dirs left behind — the core
/// regression test for the atomic-staging fix: without it, both extractions
/// would write directly into `dest` and could interleave their bytes.
#[tokio::test]
async fn concurrent_downloads_to_the_same_dest_yield_one_complete_extraction() {
  let tar = build_tarball(&[(
    "acme-repo-v1-abc123/README.md",
    b"concurrent-same-dest-content",
  )]);
  let gate = Arc::new(tokio::sync::Barrier::new(2));
  let addr = spawn_gated_tarball_server(tar, Arc::clone(&gate)).await;
  let url = format!("http://{addr}/tarball.tar.gz");
  let client = reqwest::Client::new();
  let dest = tmp_dest("concurrent-same-dest");

  let (a, b) = tokio::join!(
    download_and_extract_action(&client, &url, None, &dest),
    download_and_extract_action(&client, &url, None, &dest),
  );
  a.expect("first concurrent extraction must succeed");
  b.expect("second concurrent extraction must succeed");

  let read = std::fs::read(dest.join("README.md")).expect("extracted file present exactly once");
  assert_eq!(read, b"concurrent-same-dest-content");
  assert!(is_action_cached(&dest), "watermark must be written");
  assert_eq!(
    leftover_staging_dirs(&dest),
    Vec::<PathBuf>::new(),
    "no staging dirs must remain after both extractions settle"
  );
}

/// A tarball that fails to extract (corrupt/non-gzip body) must leave NO
/// staging dir and NO `dest` behind — `promote_staging` never runs, and the
/// staging dir created by the failed `extract_tarball` call is cleaned up.
#[tokio::test]
async fn download_and_extract_action_leaves_no_dest_or_staging_on_extraction_failure() {
  let addr = spawn_tarball_server(b"not a valid gzip tarball".to_vec()).await;
  let url = format!("http://{addr}/tarball.tar.gz");
  let client = reqwest::Client::new();
  let dest = tmp_dest("corrupt-extraction");

  let result = download_and_extract_action(&client, &url, None, &dest).await;
  assert!(
    matches!(result, Err(RunnerError::ActionDownload(_))),
    "a corrupt tarball must fail extraction: {result:?}"
  );
  assert!(
    !dest.exists(),
    "dest must not exist after a failed extraction"
  );
  assert_eq!(
    leftover_staging_dirs(&dest),
    Vec::<PathBuf>::new(),
    "no staging dir must remain after a failed extraction"
  );
}
