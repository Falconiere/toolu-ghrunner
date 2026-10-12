//! Tests for `runtime`: pins and layout helpers, the authoritative preseed
//! directory (hit with the real host `node` and no network, miss, wrong
//! major, non-Node binary), and true end-to-end downloads through
//! `ensure_node_runtime_from` against a real local HTTP server serving
//! tarballs built from the real host `node`: committed only after it starts,
//! rejected without a cache dir when the binary cannot (real `/bin/echo`),
//! the alpine `./` layout for a container target, and 404 / corrupt /
//! truncated bodies leaving nothing behind.

use super::*;
use flate2::Compression;
use flate2::write::GzEncoder;
use std::io::Write;
use std::net::SocketAddr;
use tar::{Builder, Header};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Nothing listens on the discard port: any request to it fails fast, so a
/// preseed hit that returns `Ok` provably never touched the network.
const UNROUTABLE: DownloadBases<'static> = DownloadBases {
  nodejs: "http://127.0.0.1:9",
  alpine: "http://127.0.0.1:9",
};

const GLIBC_X64: NodeTarget = NodeTarget {
  os: NodeOs::Linux,
  arch: NodeArch::X64,
  libc: Libc::Glibc,
};

/// The host's real `node` on `PATH` and its major (a host without one fails).
fn host_node() -> Result<(PathBuf, u8), Box<dyn std::error::Error>> {
  let path = std::env::var_os("PATH").ok_or("PATH unset")?;
  let node = std::env::split_paths(&path)
    .map(|dir| dir.join("node"))
    .find(|candidate| candidate.is_file())
    .ok_or("no `node` on PATH; the runtime tests need a real Node.js")?;
  let output = std::process::Command::new(&node)
    .arg("--version")
    .output()?;
  let version = String::from_utf8(output.stdout)?;
  let major = version
    .trim()
    .trim_start_matches('v')
    .split('.')
    .next()
    .ok_or("empty node version")?
    .parse()?;
  Ok((node, major))
}

fn config_for(data_dir: &Path, node_dir: Option<PathBuf>) -> RunnerConfig {
  RunnerConfig {
    data_dir: data_dir.to_path_buf(),
    node_dir,
    ..RunnerConfig::default()
  }
}

/// A preseed dir holding `binary` as `<dir>/<name>/bin/node`.
fn preseed(dir: &Path, name: &str, binary: &Path) -> TestResult {
  let bin = dir.join(name).join("bin");
  std::fs::create_dir_all(&bin)?;
  std::fs::copy(binary, bin.join("node"))?;
  Ok(())
}

/// A gzip tarball (stored blocks, so the real ~100 MB binary packs fast)
/// holding `binary` as `<prefix>bin/node`.
fn tarball_with(prefix: &str, binary: &Path) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
  let mut raw = Vec::new();
  {
    let mut builder = Builder::new(&mut raw);
    let body = std::fs::read(binary)?;
    let mut header = Header::new_gnu();
    header.set_size(u64::try_from(body.len())?);
    header.set_mode(0o755);
    header.set_cksum();
    builder.append_data(&mut header, format!("{prefix}bin/node"), &body[..])?;
    builder.finish()?;
  }
  let mut gz = GzEncoder::new(Vec::new(), Compression::none());
  gz.write_all(&raw)?;
  Ok(gz.finish()?)
}

/// Serve `status` + `body` for every path on a random local port.
async fn serve(
  status: axum::http::StatusCode,
  body: Vec<u8>,
) -> Result<SocketAddr, std::io::Error> {
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
  let addr = listener.local_addr()?;
  let app = axum::Router::new().fallback(move || {
    let body = body.clone();
    async move { (status, body) }
  });
  tokio::spawn(async move { axum::serve(listener, app).await });
  Ok(addr)
}

fn bases(addr: SocketAddr) -> (String, String) {
  (
    format!("http://{addr}/dist"),
    format!("http://{addr}/alpine"),
  )
}

#[test]
fn pins_match_upstream_externals() {
  assert_eq!(node_version_for(20), "20.20.2");
  assert_eq!(node_version_for(24), "24.21.0");
  assert_eq!(node_version_for(99), "20.20.2", "unknown majors fall back");
}

#[test]
fn layout_helpers_join_correctly() {
  let data = Path::new("/data");
  assert_eq!(
    node_cache_dir(data, "20.20.2"),
    Path::new("/data/node/20.20.2")
  );
  let musl = NodeTarget {
    libc: Libc::Musl,
    ..GLIBC_X64
  };
  assert_eq!(
    node_runtime_dir(data, "24.21.0", musl),
    Path::new("/data/node/24.21.0-alpine")
  );
  assert_eq!(
    preseed_binary_path(Path::new("/opt/externals"), 24, musl),
    Path::new("/opt/externals/node24_alpine/bin/node")
  );
}

#[test]
fn download_urls_follow_each_source_layout() {
  let (url, strip) = node_download_url(&PRODUCTION_BASES, "24.21.0", GLIBC_X64);
  assert_eq!(
    (url.as_str(), strip),
    (
      "https://nodejs.org/dist/v24.21.0/node-v24.21.0-linux-x64.tar.gz",
      1
    )
  );
  let darwin = NodeTarget {
    os: NodeOs::Darwin,
    arch: NodeArch::Arm64,
    libc: Libc::Glibc,
  };
  assert_eq!(
    node_download_url(&PRODUCTION_BASES, "20.20.2", darwin).0,
    "https://nodejs.org/dist/v20.20.2/node-v20.20.2-darwin-arm64.tar.gz"
  );
  let musl = NodeTarget {
    libc: Libc::Musl,
    ..GLIBC_X64
  };
  let (url, strip) = node_download_url(&PRODUCTION_BASES, "20.20.2", musl);
  assert_eq!(
    (url.as_str(), strip),
    (
      "https://github.com/actions/alpine_nodejs/releases/download/v20.20.2/node-v20.20.2-alpine-x64.tar.gz",
      0
    )
  );
}

#[tokio::test]
async fn preseed_hit_returns_the_real_node_without_network() -> TestResult {
  let (node, major) = host_node()?;
  let dir = tempfile::tempdir()?;
  let data = tempfile::tempdir()?;
  preseed(dir.path(), &format!("node{major}"), &node)?;
  let config = config_for(data.path(), Some(dir.path().to_path_buf()));
  let client = reqwest::Client::new();
  let binary =
    ensure_node_runtime_from(&UNROUTABLE, &client, &config, major, GLIBC_X64, true).await?;
  assert_eq!(binary, dir.path().join(format!("node{major}/bin/node")));
  assert!(
    !data.path().join("node").exists(),
    "the cache is never used"
  );
  Ok(())
}

#[tokio::test]
async fn preseed_miss_names_the_expected_path_and_never_downloads() -> TestResult {
  let dir = tempfile::tempdir()?;
  let data = tempfile::tempdir()?;
  let config = config_for(data.path(), Some(dir.path().to_path_buf()));
  let musl = NodeTarget {
    libc: Libc::Musl,
    ..GLIBC_X64
  };
  let client = reqwest::Client::new();
  let result = ensure_node_runtime_from(&UNROUTABLE, &client, &config, 24, musl, false).await;
  let expected = dir.path().join("node24_alpine/bin/node");
  assert_eq!(
    result.map_err(|e| e.to_string()),
    Err(format!(
      "node runtime error: Node.js runtime node24_alpine for linux-x64 (musl) not found in \
       TOOLU_RUNNER_NODE_DIR: expected {}",
      expected.display()
    ))
  );
  Ok(())
}

#[tokio::test]
async fn preseed_with_the_wrong_major_or_a_non_node_binary_fails_validation() -> TestResult {
  let (node, major) = host_node()?;
  let other = major.wrapping_add(1);
  let dir = tempfile::tempdir()?;
  let data = tempfile::tempdir()?;
  preseed(dir.path(), &format!("node{other}"), &node)?;
  preseed(dir.path(), &format!("node{major}"), Path::new("/bin/echo"))?;
  let config = config_for(data.path(), Some(dir.path().to_path_buf()));
  let client = reqwest::Client::new();
  let mislabeled =
    ensure_node_runtime_from(&UNROUTABLE, &client, &config, other, GLIBC_X64, true).await;
  let message = mislabeled
    .map_err(|e| e.to_string())
    .err()
    .ok_or("must fail")?;
  assert!(
    message.contains(&format!("expected v{other}.x")),
    "{message}"
  );
  let echo = ensure_node_runtime_from(&UNROUTABLE, &client, &config, major, GLIBC_X64, true).await;
  let message = echo.map_err(|e| e.to_string()).err().ok_or("must fail")?;
  // A multicall coreutils `echo` refuses to run as `node`; a plain one
  // prints its banner. Either way the runtime is rejected.
  assert!(
    message.contains("reported version") || message.contains("failed to start"),
    "{message}"
  );
  Ok(())
}

#[tokio::test]
async fn download_commits_a_real_node_only_after_it_starts() -> TestResult {
  let (node, major) = host_node()?;
  let version = node_version_for(major);
  let tar = tarball_with(&format!("node-v{version}-linux-x64/"), &node)?;
  let addr = serve(axum::http::StatusCode::OK, tar).await?;
  let (nodejs, alpine) = bases(addr);
  let data = tempfile::tempdir()?;
  let config = config_for(data.path(), None);
  let client = reqwest::Client::new();
  let bases = DownloadBases {
    nodejs: &nodejs,
    alpine: &alpine,
  };
  let binary = ensure_node_runtime_from(&bases, &client, &config, major, GLIBC_X64, true).await?;
  assert_eq!(
    binary,
    node_binary_path(&node_cache_dir(data.path(), version))
  );
  let output = std::process::Command::new(&binary)
    .arg("--version")
    .output()?;
  assert!(String::from_utf8(output.stdout)?.starts_with(&format!("v{major}.")));
  assert_eq!(staging_leftovers(data.path())?, Vec::<String>::new());
  Ok(())
}

#[tokio::test]
#[cfg(unix)]
async fn a_downloaded_binary_that_cannot_run_as_node_is_never_committed() -> TestResult {
  let tar = tarball_with("node-v20.20.2-linux-x64/", Path::new("/bin/echo"))?;
  let addr = serve(axum::http::StatusCode::OK, tar).await?;
  let (nodejs, alpine) = bases(addr);
  let data = tempfile::tempdir()?;
  let config = config_for(data.path(), None);
  let bases = DownloadBases {
    nodejs: &nodejs,
    alpine: &alpine,
  };
  let result = ensure_node_runtime_from(
    &bases,
    &reqwest::Client::new(),
    &config,
    20,
    GLIBC_X64,
    true,
  )
  .await;
  let message = result.map_err(|e| e.to_string()).err().ok_or("must fail")?;
  assert!(
    message.contains("was not installed")
      && (message.contains("reported version") || message.contains("failed to start")),
    "{message}"
  );
  assert!(!node_cache_dir(data.path(), "20.20.2").exists());
  assert_eq!(staging_leftovers(data.path())?, Vec::<String>::new());
  Ok(())
}

#[tokio::test]
async fn container_target_alpine_layout_installs_without_a_host_start_check() -> TestResult {
  // Container-only targets are not executed on the host, so any binary in
  // the alpine `./` layout commits; the container lane proves it runs.
  let tar = tarball_with("./", Path::new("/bin/echo"))?;
  let addr = serve(axum::http::StatusCode::OK, tar).await?;
  let (nodejs, alpine) = bases(addr);
  let data = tempfile::tempdir()?;
  let config = config_for(data.path(), None);
  let musl = NodeTarget {
    libc: Libc::Musl,
    ..GLIBC_X64
  };
  let bases = DownloadBases {
    nodejs: &nodejs,
    alpine: &alpine,
  };
  let binary =
    ensure_node_runtime_from(&bases, &reqwest::Client::new(), &config, 24, musl, false).await?;
  assert_eq!(binary, data.path().join("node/24.21.0-alpine/bin/node"));
  assert!(binary.is_file());
  Ok(())
}

#[tokio::test]
async fn http_error_corrupt_and_truncated_bodies_leave_no_cache_dir() -> TestResult {
  let (node, _) = host_node()?;
  let real = tarball_with("node-v20.20.2-linux-x64/", &node)?;
  let truncated = real
    .get(..real.len() / 2)
    .ok_or("tarball too small")?
    .to_vec();
  let cases = [
    (axum::http::StatusCode::NOT_FOUND, Vec::new()),
    (
      axum::http::StatusCode::OK,
      b"not a valid gzip tarball".to_vec(),
    ),
    (axum::http::StatusCode::OK, truncated),
  ];
  for (status, body) in cases {
    let addr = serve(status, body).await?;
    let (nodejs, alpine) = bases(addr);
    let data = tempfile::tempdir()?;
    let config = config_for(data.path(), None);
    let bases = DownloadBases {
      nodejs: &nodejs,
      alpine: &alpine,
    };
    let result = ensure_node_runtime_from(
      &bases,
      &reqwest::Client::new(),
      &config,
      20,
      GLIBC_X64,
      true,
    )
    .await;
    assert!(result.is_err(), "{status} must fail");
    assert!(!node_cache_dir(data.path(), "20.20.2").exists(), "{status}");
    assert_eq!(
      staging_leftovers(data.path())?,
      Vec::<String>::new(),
      "{status}"
    );
  }
  Ok(())
}

/// Names under `<data>/node` that look like staging dirs.
fn staging_leftovers(data: &Path) -> Result<Vec<String>, std::io::Error> {
  let node = data.join("node");
  if !node.exists() {
    return Ok(Vec::new());
  }
  Ok(
    std::fs::read_dir(node)?
      .filter_map(Result::ok)
      .map(|entry| entry.file_name().to_string_lossy().into_owned())
      .filter(|name| name.contains(".tmp-"))
      .collect(),
  )
}
