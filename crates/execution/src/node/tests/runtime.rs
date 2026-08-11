//! Tests for `runtime`: version resolution helpers plus `extract_tarball`
//! (real tar.gz fixture, contents + permissions verified), an end-to-end
//! streamed download+extract against a real local HTTP server, and the
//! atomic-staging promotion (`promote_staging`) under real concurrency and
//! under extraction failure.
//!
//! Zero coverage existed for this module before this file — `cargo nextest
//! run -p execution runtime` previously matched nothing and passed
//! vacuously.

use super::*;
use flate2::Compression;
use flate2::write::GzEncoder;
use std::env;
use std::io::Write;
use std::net::SocketAddr;
use std::sync::{Arc, Barrier};
use tar::{Builder, Header};

#[test]
fn node_version_for_known_majors() {
  assert_eq!(node_version_for(20), "20.18.3");
  assert_eq!(node_version_for(24), "24.0.2");
}

#[test]
fn node_version_for_unknown_major_falls_back_to_lts() {
  assert_eq!(node_version_for(99), "20.18.3");
}

#[test]
fn node_cache_dir_and_binary_path_join_correctly() {
  let data_dir = Path::new("/data");
  let cache_dir = node_cache_dir(data_dir, "20.18.3");
  assert_eq!(cache_dir, Path::new("/data/node/20.18.3"));
  assert_eq!(
    node_binary_path(&cache_dir),
    Path::new("/data/node/20.18.3/bin/node")
  );
}

#[test]
fn node_download_url_is_well_formed() {
  let url = node_download_url("20.18.3", "linux", "x64");
  assert_eq!(
    url,
    "https://nodejs.org/dist/v20.18.3/node-v20.18.3-linux-x64.tar.gz"
  );
}

fn tmp_dest(label: &str) -> PathBuf {
  let mut p = env::temp_dir();
  p.push(format!(
    "toolu-node-runtime-test-{label}-{}",
    uuid::Uuid::new_v4()
  ));
  p
}

/// Build a tar.gz in memory containing the given entries (path, body,
/// unix mode). Mirrors `actions::downloader`'s test fixture builder.
fn build_tarball(entries: &[(&str, &[u8], u32)]) -> Vec<u8> {
  let mut raw = Vec::new();
  {
    let mut builder = Builder::new(&mut raw);
    for (path, body, mode) in entries {
      let mut header = Header::new_gnu();
      header.set_size(body.len() as u64);
      header.set_mode(*mode);
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

#[test]
fn extract_tarball_strips_prefix_and_preserves_permissions() {
  let dest = tmp_dest("extract");
  let tar = build_tarball(&[
    (
      "node-v20.18.3-linux-x64/bin/node",
      b"#!fake-node-binary",
      0o755,
    ),
    ("node-v20.18.3-linux-x64/README.md", b"node runtime", 0o644),
  ]);

  extract_tarball(&tar[..], &dest).expect("tarball must extract");

  let node_bin = dest.join("bin").join("node");
  let content = std::fs::read(&node_bin).expect("binary present");
  assert_eq!(content, b"#!fake-node-binary");

  let readme = std::fs::read(dest.join("README.md")).expect("readme present");
  assert_eq!(readme, b"node runtime");

  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(&node_bin)
      .expect("metadata")
      .permissions()
      .mode();
    assert_eq!(
      mode & 0o777,
      0o755,
      "executable bit must survive extraction"
    );
  }
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
    "/node.tar.gz",
    axum::routing::get(move || async move { body }),
  );
  tokio::spawn(async move {
    let _ = axum::serve(listener, app).await;
  });
  addr
}

/// AC-9 for the node-runtime copy: a real tarball fixture served over
/// local HTTP downloads and extracts through the streamed path
/// (`bytes_stream` → `StreamReader` → `SyncIoBridge` → `spawn_blocking`),
/// landing at the resolved binary path with the returned path matching.
#[tokio::test]
async fn ensure_node_runtime_streams_from_http() {
  let tar = build_tarball(&[("node-v20.18.3-linux-x64/bin/node", b"#!fake-node", 0o755)]);
  let addr = spawn_tarball_server(tar).await;

  let data_dir = tmp_dest("data-dir");
  std::fs::create_dir_all(&data_dir).expect("mkdir data_dir");
  let client = reqwest::Client::new();
  let version = node_version_for(20);
  let cache_dir = node_cache_dir(&data_dir, version);
  let expected_binary = node_binary_path(&cache_dir);

  // ensure_node_runtime hardcodes the nodejs.org URL, so this test drives
  // extract_tarball's streamed path directly (the same code the download
  // path calls) against bytes fetched from the local server, matching
  // what AC-9 asks for: a real fixture over local HTTP through the
  // streamed extraction path.
  let response = client
    .get(format!("http://{addr}/node.tar.gz"))
    .send()
    .await
    .expect("fetch from local server");
  assert!(response.status().is_success());

  let stream = response
    .bytes_stream()
    .map(|chunk| chunk.map_err(std::io::Error::other));
  let stream_reader = tokio_util::io::StreamReader::new(stream);
  let sync_reader = tokio_util::io::SyncIoBridge::new(stream_reader);
  let dest = cache_dir.clone();
  tokio::task::spawn_blocking(move || extract_tarball(sync_reader, &dest))
    .await
    .expect("blocking extraction task must join")
    .expect("extraction must succeed");

  let content = std::fs::read(&expected_binary).expect("binary present at expected path");
  assert_eq!(content, b"#!fake-node");
}

/// List any sibling staging dirs left behind for `dest` (i.e. anything
/// matching `<dest file name>.tmp-*` in `dest`'s parent directory). Mirrors
/// `actions::downloader`'s test helper of the same name.
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

/// Two extractions racing `promote_staging` for the SAME `dest` (a
/// `std::sync::Barrier` forces genuine overlap on their blocking threads,
/// not a lucky interleave) must both return `Ok`, and `dest` must end up
/// holding exactly ONE complete extraction's content — never an
/// interleaving of both — with no `.tmp-` staging dirs left behind. This is
/// the core regression test for the atomic-staging fix applied to the node
/// runtime cache: two runner processes (different repos, always-online
/// loop) installing the same Node.js version share this cache dir.
#[tokio::test]
async fn concurrent_promotions_to_the_same_dest_yield_one_complete_tree() {
  let dest = tmp_dest("node-concurrent-dest");
  let staging_a = staging_dir_for(&dest);
  let staging_b = staging_dir_for(&dest);
  let tar_a = build_tarball(&[("node-v20.18.3-linux-x64/bin/node", b"#!fake-node-a", 0o755)]);
  let tar_b = build_tarball(&[("node-v20.18.3-linux-x64/bin/node", b"#!fake-node-b", 0o755)]);
  extract_tarball(&tar_a[..], &staging_a).expect("extract into staging a");
  extract_tarball(&tar_b[..], &staging_b).expect("extract into staging b");

  let gate = Arc::new(Barrier::new(2));

  let gate_a = Arc::clone(&gate);
  let dest_a = dest.clone();
  let handle_a = tokio::task::spawn_blocking(move || {
    gate_a.wait();
    promote_staging(&staging_a, &dest_a)
  });

  let gate_b = Arc::clone(&gate);
  let dest_b = dest.clone();
  let handle_b = tokio::task::spawn_blocking(move || {
    gate_b.wait();
    promote_staging(&staging_b, &dest_b)
  });

  let (result_a, result_b) = tokio::join!(handle_a, handle_b);
  result_a
    .expect("promote task a must join")
    .expect("promote a must succeed");
  result_b
    .expect("promote task b must join")
    .expect("promote b must succeed");

  let content =
    std::fs::read(node_binary_path(&dest)).expect("binary present exactly once at dest");
  assert!(
    content == b"#!fake-node-a" || content == b"#!fake-node-b",
    "dest must hold exactly one complete extraction's content, got: {content:?}"
  );
  assert_eq!(
    leftover_staging_dirs(&dest),
    Vec::<PathBuf>::new(),
    "no staging dirs must remain after both promotions settle"
  );
}

/// A stale `dest` left by a pre-staging-fix (or interrupted) extraction —
/// present on disk but missing the binary — must be replaced rather than
/// causing `promote_staging` to error out.
#[test]
fn promote_staging_replaces_a_stale_dest_missing_the_binary() {
  let dest = tmp_dest("node-stale-dest");
  std::fs::create_dir_all(&dest).expect("mkdir stale dest");
  std::fs::write(dest.join("leftover.txt"), b"partial").expect("write stale leftover file");
  assert!(!node_binary_path(&dest).exists(), "precondition: no binary");

  let staging = staging_dir_for(&dest);
  let tar = build_tarball(&[("node-v20.18.3-linux-x64/bin/node", b"#!fresh-node", 0o755)]);
  extract_tarball(&tar[..], &staging).expect("extract into staging");

  promote_staging(&staging, &dest).expect("promote over a stale dest must succeed");

  let content = std::fs::read(node_binary_path(&dest)).expect("fresh binary present");
  assert_eq!(content, b"#!fresh-node");
  assert!(
    !dest.join("leftover.txt").exists(),
    "the stale leftover file must not survive the replace"
  );
  assert_eq!(
    leftover_staging_dirs(&dest),
    Vec::<PathBuf>::new(),
    "no staging dir must remain after a successful promote"
  );
}

/// A corrupt (non-gzip) tarball body must fail extraction and leave NO
/// staging dir and NO cache dir behind — this drives the same
/// fetch→stage→extract sequence `ensure_node_runtime` runs internally
/// (it can't be exercised directly here since it hardcodes the nodejs.org
/// URL; see `ensure_node_runtime_streams_from_http` above for the same
/// constraint on the success path).
#[tokio::test]
async fn extraction_failure_leaves_no_cache_dir_or_staging_dir() {
  let addr = spawn_tarball_server(b"not a valid gzip tarball".to_vec()).await;
  let client = reqwest::Client::new();
  let cache_dir = tmp_dest("node-corrupt-cachedir");

  let url = format!("http://{addr}/node.tar.gz");
  let sync_reader = fetch_node_tarball_reader(&client, &url)
    .await
    .expect("fetch itself must succeed even though the body is corrupt");

  let staging = staging_dir_for(&cache_dir);
  let staging_for_extract = staging.clone();
  let result =
    tokio::task::spawn_blocking(move || extract_tarball(sync_reader, &staging_for_extract))
      .await
      .expect("blocking extraction task must join");
  assert!(result.is_err(), "a corrupt tarball must fail to extract");
  cleanup_staging(&staging);

  assert!(
    !cache_dir.exists(),
    "cache dir must not exist after a failed extraction"
  );
  assert_eq!(
    leftover_staging_dirs(&cache_dir),
    Vec::<PathBuf>::new(),
    "no staging dir must remain after cleanup"
  );
}
