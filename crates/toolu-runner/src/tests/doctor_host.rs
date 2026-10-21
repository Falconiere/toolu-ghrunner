//! Tests for the `doctor` install/host rows: release comparison against the
//! captured real `releases/latest` body (also served over a real loopback
//! socket), disk rows measured on real directories, the nearest existing
//! ancestor for the disk probe, and the platform / uid rows.

use std::cmp::Ordering;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::{
  INSTALL_ONE_LINER, compare_release, disk_level, disk_row, disk_rows, latest_tag,
  nearest_existing, platform_row, uid_row, update_check, update_row, user_row, version_row,
};
use crate::doctor_report::{Level, Row};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const GIB: u64 = 1024 * 1024 * 1024;
const RELEASES_LATEST: &str = include_str!("../../tests/fixtures/doctor/releases_latest.json");
const USER_200: &str = include_str!("../../tests/fixtures/doctor/github_user_200.json");
const USER_401: &str = include_str!("../../tests/fixtures/doctor/github_user_401.json");

/// `tag_name` of the captured real `GET …/releases/latest` body.
fn fixture_tag() -> Result<String, Box<dyn std::error::Error>> {
  let body: serde_json::Value = serde_json::from_str(RELEASES_LATEST)?;
  Ok(
    body
      .get("tag_name")
      .and_then(serde_json::Value::as_str)
      .ok_or("fixture has no tag_name")?
      .to_owned(),
  )
}

/// Config mode with a proxy nothing listens on: loopback is always bypassed,
/// so requests go direct and the developer's `HTTP(S)_PROXY` never applies.
fn hermetic_network() -> shared::NetworkConfig {
  shared::NetworkConfig {
    proxy: Some("http://127.0.0.1:9".to_owned()),
    no_proxy: Vec::new(),
    ca_bundle: None,
  }
}

/// A one-shot loopback HTTP peer: the thread that serves the first connection.
struct Peer {
  handle: std::thread::JoinHandle<std::io::Result<String>>,
}

impl Peer {
  /// Wait for the peer; the request head it received.
  fn head(self) -> Result<String, Box<dyn std::error::Error>> {
    match self.handle.join() {
      Ok(head) => Ok(head?),
      Err(_panic) => Err("the loopback peer thread panicked".into()),
    }
  }
}

/// Serve `status` and `body` to the first connection on a loopback port.
/// Returns its base URL and the peer.
fn serve_once(status: &str, body: &str) -> Result<(String, Peer), Box<dyn std::error::Error>> {
  let listener = TcpListener::bind("127.0.0.1:0")?;
  listener.set_nonblocking(true)?;
  let base = format!("http://{}", listener.local_addr()?);
  let response = format!(
    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
    body.len()
  );
  let handle = std::thread::spawn(move || {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut stream = loop {
      match listener.accept() {
        Ok((stream, _)) => break stream,
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
          if Instant::now() > deadline {
            return Err(std::io::Error::new(
              std::io::ErrorKind::TimedOut,
              "no client connected within 10 s",
            ));
          }
          std::thread::sleep(Duration::from_millis(5));
        },
        Err(error) => return Err(error),
      }
    };
    stream.set_nonblocking(false)?;
    let mut head = Vec::new();
    let mut chunk = [0_u8; 1024];
    while !head.windows(4).any(|window| window == b"\r\n\r\n") {
      let read = stream.read(&mut chunk)?;
      if read == 0 {
        break;
      }
      head.extend_from_slice(chunk.get(..read).unwrap_or_default());
    }
    stream.write_all(response.as_bytes())?;
    Ok(String::from_utf8_lossy(&head).into_owned())
  });
  Ok((base, Peer { handle }))
}

/// Whether the request head has `line` (case-insensitively).
fn has_line(head: &str, line: &str) -> bool {
  head.lines().any(|got| got.eq_ignore_ascii_case(line))
}

/// `X.Y GiB`-style figure `doctor` prints for `free` bytes.
fn gib_shown(free: u64) -> String {
  format!("{:.1}", free as f64 / GIB as f64)
}

/// The `disk` row `doctor` must print for `path` when `free` bytes are free.
fn disk_row_for(path: &Path, free: u64) -> Row {
  let level = disk_level(free);
  let row = Row::new(
    level,
    "disk",
    format!("{} GiB free at {}", gib_shown(free), path.display()),
  );
  if level == Level::Ok {
    row
  } else {
    row.with_fix(format!(
      "free space on the volume holding {}",
      path.display()
    ))
  }
}

#[test]
fn compare_release_orders_numeric_versions_against_the_real_tag() -> TestResult {
  let tag = fixture_tag()?;
  assert_eq!(tag, "v0.11.1");
  assert_eq!(compare_release("0.11.1", &tag), Some(Ordering::Equal));
  assert_eq!(compare_release("0.12.0", &tag), Some(Ordering::Greater));
  assert_eq!(compare_release("0.11.10", &tag), Some(Ordering::Greater));
  assert_eq!(compare_release("0.10.0", &tag), Some(Ordering::Less));
  assert_eq!(compare_release("0.11.1", "v1.x"), None);
  assert_eq!(compare_release("0.11.1-rc.1", &tag), None);
  assert_eq!(compare_release("0.11.1", "0.11.1"), Some(Ordering::Equal));
  assert_eq!(compare_release("0.11.1", "0.11.1.2"), None);
  assert_eq!(compare_release("0.11", &tag), None);
  Ok(())
}

#[test]
fn an_older_build_warns_with_the_install_one_liner() -> TestResult {
  let tag = fixture_tag()?;
  assert_eq!(
    update_row("0.10.0", &tag),
    Row::new(
      Level::Warn,
      "update",
      "v0.11.1 is available (installed 0.10.0)"
    )
    .with_fix(INSTALL_ONE_LINER)
  );
  assert_eq!(
    INSTALL_ONE_LINER,
    "curl -fsSL https://get.toolu.sh/pkg/ghrunner/install | bash"
  );
  assert_eq!(
    update_row("0.11.1", &tag),
    Row::new(Level::Ok, "update", "up to date (latest v0.11.1)")
  );
  assert_eq!(
    update_row("0.12.0", &tag),
    Row::new(
      Level::Ok,
      "update",
      "newer than the latest release (v0.11.1)"
    )
  );
  assert_eq!(
    update_row("0.11.1", "nightly"),
    Row::new(
      Level::Skip,
      "update",
      "cannot compare 0.11.1 with the latest release nightly"
    )
  );
  Ok(())
}

#[tokio::test]
async fn an_offline_update_check_makes_no_request() {
  assert_eq!(
    update_check(
      true,
      Duration::from_secs(1),
      &shared::NetworkConfig::default()
    )
    .await,
    Row::new(Level::Skip, "update", "offline")
  );
}

#[tokio::test]
async fn latest_tag_reads_the_real_release_document_over_a_socket() -> TestResult {
  let (base, request) = serve_once("200 OK", RELEASES_LATEST)?;
  let url = format!("{base}/releases/latest");
  let tag = latest_tag(&url, Duration::from_secs(5), &hermetic_network()).await;
  assert_eq!(tag, Ok(fixture_tag()?));

  let head = request.head()?;
  assert!(has_line(&head, "GET /releases/latest HTTP/1.1"), "{head}");
  assert!(
    has_line(&head, "accept: application/vnd.github+json"),
    "{head}"
  );
  assert!(
    has_line(
      &head,
      concat!(
        "user-agent: toolu-runner/",
        env!("CARGO_PKG_VERSION"),
        " (check)"
      )
    ),
    "{head}"
  );
  Ok(())
}

#[tokio::test]
async fn latest_tag_failures_are_reasons_not_panics() -> TestResult {
  let network = hermetic_network();
  let timeout = Duration::from_secs(5);

  let (base, request) = serve_once("401 Unauthorized", USER_401)?;
  assert_eq!(
    latest_tag(&base, timeout, &network).await,
    Err("HTTP 401".to_owned())
  );
  request.head()?;

  // A real GitHub JSON document that is not a release.
  let (base, request) = serve_once("200 OK", USER_200)?;
  assert_eq!(
    latest_tag(&base, timeout, &network).await,
    Err("the response has no tag_name".to_owned())
  );
  request.head()?;

  let (base, request) = serve_once("200 OK", "<html>not json</html>")?;
  assert_eq!(
    latest_tag(&base, timeout, &network).await,
    Err("unreadable response: error decoding response body".to_owned())
  );
  request.head()?;

  // Nothing listening: bind a port, then close it.
  let closed = TcpListener::bind("127.0.0.1:0")?;
  let addr = closed.local_addr()?;
  drop(closed);
  let refused = std::net::TcpStream::connect(addr)
    .err()
    .ok_or("a closed port accepted a connection")?;
  let url = format!("http://{addr}/releases/latest?access_token=never-shown");
  // The URL is left out: a mirror's query can carry a token.
  assert_eq!(
    latest_tag(&url, timeout, &network).await,
    Err(format!(
      "connect: error sending request: client error (Connect): tcp connect error: {refused}"
    ))
  );
  Ok(())
}

#[test]
fn the_version_row_names_the_build_and_the_emulated_runner() {
  assert_eq!(
    version_row(),
    Row::new(
      Level::Ok,
      "version",
      format!(
        "toolu-runner {} (GitHub runner compatibility {})",
        env!("CARGO_PKG_VERSION"),
        protocol::runner_version::COMPATIBILITY_VERSION
      )
    )
  );
}

#[test]
fn the_platform_row_uses_githubs_runner_os_and_arch_spelling() {
  let os = if cfg!(target_os = "macos") {
    "macOS"
  } else if cfg!(target_os = "windows") {
    "Windows"
  } else {
    "Linux"
  };
  let arch = if cfg!(target_arch = "aarch64") {
    "ARM64"
  } else {
    "X64"
  };
  assert_eq!(
    platform_row(),
    Row::new(Level::Ok, "platform", format!("{os} {arch}"))
  );
}

#[test]
fn disk_thresholds_fail_below_1_gib_and_warn_below_5_gib() {
  assert_eq!(disk_level(0), Level::Fail);
  assert_eq!(disk_level(GIB - 1), Level::Fail);
  assert_eq!(disk_level(GIB), Level::Warn);
  assert_eq!(disk_level(5 * GIB - 1), Level::Warn);
  assert_eq!(disk_level(5 * GIB), Level::Ok);
}

#[test]
fn disk_rows_measure_each_distinct_path_on_its_nearest_existing_directory() -> TestResult {
  let home = tempfile::tempdir()?;
  // Never created: measured on the volume of the nearest existing ancestor.
  let work = home.path().join("_work").join("deep").join("er");
  let data = home.path().join("data");
  std::fs::create_dir(&data)?;
  let paths = [work.clone(), data.clone(), work.clone()];

  // Free space moves while other tests write; only compare a measurement
  // that did not change (at the printed precision and level) around the call.
  for _ in 0..10 {
    let before = fs2::available_space(home.path())?;
    let rows = disk_rows(&paths);
    let after = fs2::available_space(home.path())?;
    if gib_shown(before) != gib_shown(after) || disk_level(before) != disk_level(after) {
      continue;
    }
    // The repeated `work` path yields one row; both paths share the volume.
    assert_eq!(
      rows,
      vec![disk_row_for(&work, before), disk_row_for(&data, before)]
    );
    return Ok(());
  }
  Err("free space kept changing under the test".into())
}

#[test]
fn disk_rows_for_no_paths_is_empty() {
  assert_eq!(disk_rows(&[]), Vec::<Row>::new());
}

#[test]
fn nearest_existing_walks_up_to_a_real_directory() -> TestResult {
  let home = tempfile::tempdir()?;
  assert_eq!(
    nearest_existing(&home.path().join("a").join("b")),
    home.path()
  );
  assert_eq!(nearest_existing(home.path()), home.path());
  // A relative path with no existing ancestor is measured on the cwd.
  assert_eq!(
    nearest_existing(Path::new("doctor-test-missing-dir/sub")),
    PathBuf::from(".")
  );
  Ok(())
}

#[test]
fn uid_row_warns_for_root_and_unknown_uids() {
  assert_eq!(
    uid_row("0"),
    Row::new(
      Level::Warn,
      "user",
      "running as root (uid 0): every job step would run as root"
    )
    .with_fix("run the runner as a dedicated unprivileged user")
  );
  assert_eq!(uid_row("501"), Row::new(Level::Ok, "user", "uid 501"));
  for (raw, shown) in [
    ("x", "\"x\""),
    ("", "\"\""),
    ("-1", "\"-1\""),
    ("501\n", "\"501\\n\""),
  ] {
    assert_eq!(
      uid_row(raw),
      Row::new(
        Level::Warn,
        "user",
        format!("could not determine the uid ({shown})")
      ),
      "{raw:?}"
    );
  }
}

#[test]
fn the_user_row_reports_this_processs_real_uid() -> TestResult {
  let output = std::process::Command::new("id").arg("-u").output()?;
  assert!(output.status.success());
  let uid = String::from_utf8(output.stdout)?;
  assert_eq!(user_row(), uid_row(uid.trim()));
  Ok(())
}

#[test]
fn disk_rows_fail_below_one_gib_and_warn_below_five_with_the_volume_fix() {
  let path = Path::new("/srv/toolu/_work");
  let fix = Some("free space on the volume holding /srv/toolu/_work");
  let fail = disk_row(path, Ok(GIB - 1));
  assert_eq!(
    (fail.level, fail.detail.as_str(), fail.fix.as_deref()),
    (Level::Fail, "1.0 GiB free at /srv/toolu/_work", fix)
  );
  let warn = disk_row(path, Ok(5 * GIB - 1));
  assert_eq!(
    (warn.level, warn.detail.as_str(), warn.fix.as_deref()),
    (Level::Warn, "5.0 GiB free at /srv/toolu/_work", fix)
  );
  let ok = disk_row(path, Ok(5 * GIB));
  assert_eq!(
    (ok.level, ok.detail.as_str(), ok.fix.as_deref()),
    (Level::Ok, "5.0 GiB free at /srv/toolu/_work", None)
  );
}

#[test]
fn an_unmeasurable_volume_warns_with_the_real_io_error() -> Result<(), Box<dyn std::error::Error>> {
  let dir = tempfile::tempdir()?;
  let missing = dir.path().join("never-created");
  let error = fs2::available_space(&missing)
    .err()
    .ok_or("free space of a missing path was measured")?;
  let expected = format!(
    "could not measure free space at {}: {error}",
    missing.display()
  );
  let row = disk_row(&missing, fs2::available_space(&missing));
  assert_eq!(
    (row.level, row.detail.as_str(), row.fix.as_deref()),
    (Level::Warn, expected.as_str(), None)
  );
  Ok(())
}
