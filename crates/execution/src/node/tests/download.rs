//! Tests for `download`: tarball extraction (nodejs.org nesting, the
//! `actions/alpine_nodejs` `./` root, rejected escapes, permissions) and the
//! atomic staging promotion under real concurrency and over a stale dest,
//! and `install_runtime`'s retry against a real local server whose response
//! body is cut mid-stream (the dropped-connection failure seen from
//! nodejs.org).

use super::*;
use flate2::Compression;
use flate2::write::GzEncoder;
use std::env;
use std::io::Write;
use std::net::SocketAddr;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Barrier};
use tar::{Builder, Header};

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
      "node-v20.20.2-linux-x64/bin/node",
      b"#!fake-node-binary",
      0o755,
    ),
    ("node-v20.20.2-linux-x64/README.md", b"node runtime", 0o644),
  ]);

  extract_tarball(&tar[..], &dest, 1).expect("tarball must extract");

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

#[test]
fn extract_tarball_handles_the_alpine_dot_root_without_stripping() {
  let dest = tmp_dest("extract-alpine");
  // `actions/alpine_nodejs` tarballs list `./`, `./bin/`, `./bin/node`, `./LICENSE`.
  let tar = build_tarball(&[
    ("./bin/node", b"#!alpine-node", 0o755),
    ("./LICENSE", b"license", 0o644),
  ]);
  extract_tarball(&tar[..], &dest, 0).expect("alpine layout must extract");
  assert_eq!(
    std::fs::read(dest.join("bin/node")).expect("bin/node"),
    b"#!alpine-node"
  );
  assert_eq!(
    std::fs::read(dest.join("LICENSE")).expect("LICENSE"),
    b"license"
  );
}

#[test]
fn extract_tarball_rejects_an_entry_escaping_the_destination() {
  let dest = tmp_dest("extract-escape");
  let mut raw = Vec::new();
  {
    let mut builder = Builder::new(&mut raw);
    let mut header = Header::new_gnu();
    // `Header::set_path` refuses `..`, so write the raw name bytes.
    header.as_gnu_mut().expect("gnu header").name[..14].copy_from_slice(b"top/../../evil");
    header.set_size(4);
    header.set_mode(0o644);
    header.set_cksum();
    builder.append(&header, &b"evil"[..]).expect("append");
    builder.finish().expect("finish");
  }
  let mut gz = GzEncoder::new(Vec::new(), Compression::fast());
  gz.write_all(&raw).expect("gzip");
  let tar = gz.finish().expect("gzip finish");
  let error = extract_tarball(&tar[..], &dest, 1).expect_err("escape must be rejected");
  assert!(
    error.to_string().contains("escapes the runtime directory"),
    "{error}"
  );
  assert!(!dest.parent().expect("parent").join("evil").exists());
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
  let tar_a = build_tarball(&[("node-v20.20.2-linux-x64/bin/node", b"#!fake-node-a", 0o755)]);
  let tar_b = build_tarball(&[("node-v20.20.2-linux-x64/bin/node", b"#!fake-node-b", 0o755)]);
  extract_tarball(&tar_a[..], &staging_a, 1).expect("extract into staging a");
  extract_tarball(&tar_b[..], &staging_b, 1).expect("extract into staging b");

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
  let tar = build_tarball(&[("node-v20.20.2-linux-x64/bin/node", b"#!fresh-node", 0o755)]);
  extract_tarball(&tar[..], &staging, 1).expect("extract into staging");

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

/// Serve `status` + `tarball` on every path, counting requests in `hits`.
/// The first `broken` responses send half the body and then fail the
/// stream, which aborts the connection mid-body.
async fn serve_flaky(
  status: axum::http::StatusCode,
  tarball: Vec<u8>,
  broken: usize,
  hits: Arc<AtomicUsize>,
) -> SocketAddr {
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
    .await
    .expect("bind local server");
  let addr = listener.local_addr().expect("local addr");
  let app = axum::Router::new().fallback(move || {
    let tarball = tarball.clone();
    let hit = hits.fetch_add(1, Ordering::SeqCst);
    async move {
      if hit >= broken {
        return (status, axum::body::Body::from(tarball));
      }
      let half = tarball
        .get(..tarball.len() / 2)
        .unwrap_or_default()
        .to_vec();
      let chunks: Vec<Result<axum::body::Bytes, std::io::Error>> = vec![
        Ok(half.into()),
        Err(std::io::Error::other("connection dropped")),
      ];
      (
        status,
        axum::body::Body::from_stream(futures_util::stream::iter(chunks)),
      )
    }
  });
  tokio::spawn(async move { axum::serve(listener, app).await });
  addr
}

/// A tarball big enough that half of it is a partial body, not just headers.
fn sizeable_tarball() -> Vec<u8> {
  let filler = vec![b'x'; 256 * 1024];
  build_tarball(&[
    ("node-v24.21.0-linux-x64/bin/node", b"#!fake-node", 0o755),
    ("node-v24.21.0-linux-x64/filler", &filler, 0o644),
  ])
}

async fn install_from(addr: SocketAddr, cache_dir: &Path) -> Result<(), RunnerError> {
  let url = format!("http://{addr}/dist/node.tar.gz");
  install_runtime(
    &reqwest::Client::new(),
    &Install {
      url: &url,
      strip: 1,
      cache_dir,
      validate_major: None,
    },
  )
  .await
}

#[tokio::test]
async fn a_body_cut_mid_stream_is_retried_and_committed() {
  let hits = Arc::new(AtomicUsize::new(0));
  let addr = serve_flaky(
    axum::http::StatusCode::OK,
    sizeable_tarball(),
    1,
    Arc::clone(&hits),
  )
  .await;
  let dest = tmp_dest("node-retry-ok");

  install_from(addr, &dest)
    .await
    .expect("the second attempt must install");

  assert_eq!(hits.load(Ordering::SeqCst), 2);
  assert_eq!(
    std::fs::read(node_binary_path(&dest)).expect("binary committed"),
    b"#!fake-node"
  );
  assert_eq!(leftover_staging_dirs(&dest), Vec::<PathBuf>::new());
}

#[tokio::test]
async fn a_persistently_cut_body_gives_up_after_the_attempt_cap() {
  let hits = Arc::new(AtomicUsize::new(0));
  let addr = serve_flaky(
    axum::http::StatusCode::OK,
    sizeable_tarball(),
    usize::MAX,
    Arc::clone(&hits),
  )
  .await;
  let dest = tmp_dest("node-retry-exhausted");

  let error = install_from(addr, &dest)
    .await
    .expect_err("every attempt is cut");

  assert_eq!(hits.load(Ordering::SeqCst), DOWNLOAD_ATTEMPTS as usize);
  assert!(
    error
      .to_string()
      .contains(&format!("http://{addr}/dist/node.tar.gz")),
    "{error}"
  );
  assert!(!dest.exists());
  assert_eq!(leftover_staging_dirs(&dest), Vec::<PathBuf>::new());
}

#[tokio::test]
async fn a_not_found_tarball_is_requested_once() {
  let hits = Arc::new(AtomicUsize::new(0));
  let addr = serve_flaky(
    axum::http::StatusCode::NOT_FOUND,
    Vec::new(),
    0,
    Arc::clone(&hits),
  )
  .await;
  let dest = tmp_dest("node-retry-404");

  let error = install_from(addr, &dest).await.expect_err("404 must fail");

  assert_eq!(hits.load(Ordering::SeqCst), 1);
  assert!(error.to_string().contains("status 404"), "{error}");
  assert!(!dest.exists());
}

#[tokio::test]
async fn a_server_error_is_retried() {
  let hits = Arc::new(AtomicUsize::new(0));
  let addr = serve_flaky(
    axum::http::StatusCode::SERVICE_UNAVAILABLE,
    Vec::new(),
    0,
    Arc::clone(&hits),
  )
  .await;
  let dest = tmp_dest("node-retry-503");

  let error = install_from(addr, &dest).await.expect_err("503 must fail");

  assert_eq!(hits.load(Ordering::SeqCst), DOWNLOAD_ATTEMPTS as usize);
  assert!(error.to_string().contains("status 503"), "{error}");
}
