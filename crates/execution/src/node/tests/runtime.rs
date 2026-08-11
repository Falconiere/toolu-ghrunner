//! Tests for `runtime`: version resolution helpers, `extract_tarball` (real
//! tar.gz fixture, contents + permissions verified), the atomic-staging
//! promotion (`promote_staging`) under real concurrency and under extraction
//! failure, and — via the injected-`base_url` `ensure_node_runtime_from` —
//! true end-to-end runs of the production entry point's full sequencing
//! (`fetch_node_tarball_reader` → `spawn_blocking(extract_tarball)` →
//! `cleanup_staging`/`promote_staging`, including its error mapping) against
//! a real local HTTP server, on both the success path and its two distinct
//! failure branches (non-success HTTP status, and a corrupt tarball body).
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
  let url = node_download_url("https://nodejs.org/dist", "20.18.3", "linux", "x64");
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

/// Serve `status`+`body` as the response to every request on a random
/// localhost port, returning the bound address. The caller resolves the
/// exact tarball path against the returned base URL via
/// [`node_download_url`] — the fallback route matches any path, so the
/// helper works whether the caller is asserting a success or a failure
/// response. The server task is aborted when the caller's Tokio runtime
/// shuts down at test end.
async fn spawn_node_dist_server(status: axum::http::StatusCode, body: Vec<u8>) -> SocketAddr {
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
    .await
    .expect("bind local node dist server");
  let addr = listener.local_addr().expect("local_addr");
  let app = axum::Router::new().fallback(move || {
    let body = body.clone();
    async move { (status, body) }
  });
  tokio::spawn(async move {
    let _ = axum::serve(listener, app).await;
  });
  addr
}

/// End-to-end: a real tarball fixture served over local HTTP is downloaded
/// and extracted through the ACTUAL production entry point
/// (`ensure_node_runtime_from`, which `ensure_node_runtime` calls pinned to
/// the real `nodejs.org` base) — driving its full sequencing
/// (`fetch_node_tarball_reader` → `spawn_blocking(extract_tarball)` →
/// `promote_staging`) rather than replaying the steps by hand. Asserts the
/// binary lands at the resolved cache path, stays executable, and no
/// `.tmp-` staging dir survives a successful run.
#[tokio::test]
async fn ensure_node_runtime_end_to_end_downloads_and_extracts() {
  let tar = build_tarball(&[("node-v20.18.3-linux-x64/bin/node", b"#!fake-node", 0o755)]);
  let addr = spawn_node_dist_server(axum::http::StatusCode::OK, tar).await;
  let base_url = format!("http://{addr}");

  let data_dir = tmp_dest("e2e-success-data-dir");
  std::fs::create_dir_all(&data_dir).expect("mkdir data_dir");
  let client = reqwest::Client::new();

  let binary = ensure_node_runtime_from(&base_url, &client, &data_dir, 20)
    .await
    .expect("ensure_node_runtime_from must succeed end-to-end");

  let version = node_version_for(20);
  let cache_dir = node_cache_dir(&data_dir, version);
  assert_eq!(binary, node_binary_path(&cache_dir));

  let content = std::fs::read(&binary).expect("binary present at expected path");
  assert_eq!(content, b"#!fake-node");

  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(&binary)
      .expect("metadata")
      .permissions()
      .mode();
    assert_eq!(mode & 0o111, 0o111, "binary must remain executable");
  }

  assert_eq!(
    leftover_staging_dirs(&cache_dir),
    Vec::<PathBuf>::new(),
    "no staging dir must remain after a successful end-to-end run"
  );
}

/// End-to-end failure branch 1: a non-success HTTP status (404) from the
/// download host must propagate as an error out of the entry point, and
/// must leave neither a cache dir nor a staging dir behind.
#[tokio::test]
async fn ensure_node_runtime_end_to_end_404_propagates_error_with_no_leftovers() {
  let addr = spawn_node_dist_server(axum::http::StatusCode::NOT_FOUND, Vec::new()).await;
  let base_url = format!("http://{addr}");

  let data_dir = tmp_dest("e2e-404-data-dir");
  std::fs::create_dir_all(&data_dir).expect("mkdir data_dir");
  let client = reqwest::Client::new();

  let result = ensure_node_runtime_from(&base_url, &client, &data_dir, 20).await;
  assert!(
    result.is_err(),
    "a 404 tarball response must propagate as an error"
  );

  let version = node_version_for(20);
  let cache_dir = node_cache_dir(&data_dir, version);
  assert!(
    !cache_dir.exists(),
    "cache dir must not exist after a 404 fetch failure"
  );
  assert_eq!(
    leftover_staging_dirs(&cache_dir),
    Vec::<PathBuf>::new(),
    "no staging dir must remain after a 404 fetch failure"
  );
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

/// End-to-end failure branch 2: a corrupt (non-gzip) tarball body — a
/// successful fetch (200 OK) but a failing extraction — must propagate as
/// an error out of the entry point, driving the SAME
/// fetch→stage→extract→cleanup sequence `ensure_node_runtime` runs
/// internally, and must leave neither a cache dir nor a staging dir behind.
#[tokio::test]
async fn ensure_node_runtime_end_to_end_corrupt_tarball_propagates_error_with_no_leftovers() {
  let addr = spawn_node_dist_server(
    axum::http::StatusCode::OK,
    b"not a valid gzip tarball".to_vec(),
  )
  .await;
  let base_url = format!("http://{addr}");

  let data_dir = tmp_dest("e2e-corrupt-data-dir");
  std::fs::create_dir_all(&data_dir).expect("mkdir data_dir");
  let client = reqwest::Client::new();

  let result = ensure_node_runtime_from(&base_url, &client, &data_dir, 20).await;
  assert!(result.is_err(), "a corrupt tarball must fail to extract");

  let version = node_version_for(20);
  let cache_dir = node_cache_dir(&data_dir, version);
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
