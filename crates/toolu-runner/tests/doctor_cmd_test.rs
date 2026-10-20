//! Shell-out tests for `toolu-runner doctor`: the compiled binary run against
//! real registrations (the committed GHES JIT fixture), real processes, real
//! files and loopback peers serving bodies captured from the live GitHub API.

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::time::Duration;

use fs2::FileExt;

use crate::check_servers::{OK, Recorder, run};
use crate::doctor_env::{doctor_cmd, write_registration};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// The captured real `GET …/releases/latest` body as an HTTP response.
const RELEASES_LATEST: &str = concat!(
  "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n",
  include_str!("fixtures/doctor/releases_latest.json")
);

/// One loopback peer for every GitHub host a GHES-shaped registration names:
/// the release check gets the captured body, every service endpoint a 200.
fn github_reply(head: &str) -> &'static str {
  if head.starts_with("GET /releases/latest ") {
    RELEASES_LATEST
  } else {
    OK
  }
}

#[test]
fn empty_home() -> TestResult {
  let home = tempfile::tempdir()?;
  let (code, stdout, stderr, _) = run(doctor_cmd(home.path())?.arg("--offline"))?;

  assert_eq!(code, Some(0), "stdout:\n{stdout}\nstderr:\n{stderr}");
  assert!(stdout.contains("  INFO  registrations none"), "{stdout}");
  assert!(stdout.contains("toolu-runner register"), "{stdout}");
  assert!(
    stdout.contains(&format!("free at {}", home.path().display())),
    "{stdout}"
  );
  assert!(!stdout.contains("FAIL"), "{stdout}");
  Ok(())
}

#[test]
fn update_check() -> TestResult {
  let home = tempfile::tempdir()?;
  let peer = Recorder::spawn(|head| Some(github_reply(head)))?;
  let base = format!("http://127.0.0.1:{}", peer.port());
  // Ephemeral: no bearer is needed, so the token section cannot fail.
  write_registration(home.path(), "o", "r", &base, true)?;

  let (code, stdout, stderr, _) = run(doctor_cmd(home.path())?.args(["--timeout", "5"]).env(
    "TOOLU_RUNNER_RELEASES_URL",
    format!("{base}/releases/latest"),
  ))?;
  let update = stdout
    .lines()
    .find(|line| line.starts_with("  OK    update     "))
    .ok_or_else(|| format!("no OK update row:\n{stdout}\nstderr:\n{stderr}"))?;
  // Build versions only grow, so the captured v0.11.1 is never newer.
  assert!(update.contains("v0.11.1"), "{update}");
  assert!(
    peer
      .request_lines()
      .contains(&"GET /releases/latest HTTP/1.1".to_owned()),
    "{:?}",
    peer.request_lines()
  );
  assert_eq!(code, Some(0), "stdout:\n{stdout}\nstderr:\n{stderr}");

  // The default (closed port) release URL: SKIP, and the exit is unaffected.
  let (unreachable_code, unreachable, _, _) =
    run(doctor_cmd(home.path())?.args(["--timeout", "5"]))?;
  assert!(
    unreachable.contains("  SKIP  update     could not check latest release"),
    "{unreachable}"
  );
  assert_eq!(unreachable_code, code, "{unreachable}");
  Ok(())
}

/// Every path under `dir`, relative and sorted, with file sizes — a change
/// anywhere (a new `.lock`, `_diag`, a rewritten file) changes the listing.
fn tree(dir: &Path) -> std::io::Result<Vec<String>> {
  let mut out = Vec::new();
  let mut stack = vec![dir.to_path_buf()];
  while let Some(next) = stack.pop() {
    for entry in std::fs::read_dir(&next)? {
      let entry = entry?;
      let meta = entry.metadata()?;
      let rel = entry
        .path()
        .strip_prefix(dir)
        .map(|rel| rel.display().to_string())
        .unwrap_or_default();
      out.push(format!("{rel} {}", meta.len()));
      if meta.is_dir() {
        stack.push(entry.path());
      }
    }
  }
  out.sort();
  Ok(out)
}

/// A PID that belonged to a real process which has exited and been reaped.
fn dead_pid() -> Result<u32, Box<dyn std::error::Error>> {
  for _ in 0..5 {
    let mut child = std::process::Command::new("true").spawn()?;
    let pid = child.id();
    child.wait()?;
    if !config::lockfile::is_pid_alive(pid) {
      return Ok(pid);
    }
  }
  Err("every reaped PID was reused at once".into())
}

/// Write a `.lock` body the way `run` does, naming `pid`.
fn write_lock_body(reg_dir: &Path, pid: u32, config: &Path) -> std::io::Result<()> {
  let body = config::lockfile::LockBody {
    pid,
    started_at: "2026-10-19T10:00:00Z".to_owned(),
    config_path: config.display().to_string(),
  };
  std::fs::write(
    reg_dir.join(".lock"),
    serde_json::to_string(&body).map_err(std::io::Error::other)?,
  )
}

/// The rows printed under the registration heading for `owner/repo`.
fn block<'a>(stdout: &'a str, owner_repo: &str) -> Vec<&'a str> {
  stdout
    .lines()
    .skip_while(|line| {
      !line.starts_with(&format!("  {owner_repo} (")) && *line != format!("  {owner_repo}:")
    })
    .skip(1)
    .take_while(|line| line.starts_with("  ") && !line.ends_with("):") && !line.ends_with(':'))
    .collect()
}

#[test]
fn registration_integrity() -> TestResult {
  let home = tempfile::tempdir()?;
  let base = "http://127.0.0.1:9";
  let bad = write_registration(home.path(), "o", "bad", base, true)?;
  std::fs::write(&bad, "runner_url = [unterminated")?;
  let loose = write_registration(home.path(), "o", "loose", base, true)?;
  let loose_creds = loose.with_file_name("credentials.json");
  std::fs::set_permissions(&loose_creds, std::fs::Permissions::from_mode(0o644))?;
  let missing = write_registration(home.path(), "o", "nocreds", base, true)?;
  std::fs::remove_file(missing.with_file_name("credentials.json"))?;

  let (code, stdout, stderr, _) = run(doctor_cmd(home.path())?.arg("--offline"))?;
  assert_eq!(code, Some(1), "stdout:\n{stdout}\nstderr:\n{stderr}");

  let bad_rows = block(&stdout, "o/bad");
  assert!(
    bad_rows
      .iter()
      .any(|row| row.starts_with("  FAIL  config ")),
    "{stdout}"
  );

  let loose_rows = block(&stdout, "o/loose");
  assert!(
    loose_rows
      .iter()
      .any(|row| row.starts_with("  WARN  creds ") && row.contains("0644")),
    "{stdout}"
  );
  assert!(
    loose_rows.contains(&format!("        fix: chmod 600 {}", loose_creds.display()).as_str()),
    "{stdout}"
  );
  // The loose registration's other rows are still reported.
  assert!(
    loose_rows.iter().any(|row| row.starts_with("  OK    jit ")),
    "{stdout}"
  );
  assert!(
    loose_rows
      .iter()
      .any(|row| row.starts_with("  OK    config ")),
    "{stdout}"
  );

  let missing_rows = block(&stdout, "o/nocreds");
  assert!(
    missing_rows
      .iter()
      .any(|row| row.starts_with("  FAIL  creds ")),
    "{stdout}"
  );
  Ok(())
}

#[test]
fn run_state() -> TestResult {
  let home = tempfile::tempdir()?;
  let config = write_registration(home.path(), "o", "r", "http://127.0.0.1:9", false)?;
  let reg_dir = config.parent().ok_or("no parent")?;
  write_lock_body(reg_dir, dead_pid()?, &config)?;
  std::fs::write(reg_dir.join(".pending_remove"), "")?;

  let (code, stdout, stderr, _) = run(doctor_cmd(home.path())?.arg("--offline"))?;
  assert_eq!(code, Some(1), "stdout:\n{stdout}\nstderr:\n{stderr}");
  let rows = block(&stdout, "o/r");
  assert!(rows.contains(&"  FAIL  run        not running"), "{stdout}");
  assert!(
    rows.contains(
      &format!(
        "        fix: toolu-runner install-service --config {}",
        config.display()
      )
      .as_str()
    ),
    "{stdout}"
  );
  assert!(
    rows.iter().any(|row| row.starts_with("  WARN  pending ")),
    "{stdout}"
  );
  Ok(())
}

#[test]
fn last_job() -> TestResult {
  let home = tempfile::tempdir()?;
  let config = write_registration(home.path(), "o", "r", "http://127.0.0.1:9", true)?;
  write_registration(home.path(), "o", "fresh", "http://127.0.0.1:9", true)?;
  let jobs = config.with_file_name("_diag").join("jobs");
  std::fs::create_dir_all(&jobs)?;
  std::fs::write(
    jobs.join("2026-10-11T02-01-30Z-0e9d8c7b.jsonl"),
    include_str!("fixtures/journal/canonical.jsonl"),
  )?;

  let (_, stdout, _, _) = run(doctor_cmd(home.path())?.arg("--offline"))?;
  assert!(
    block(&stdout, "o/r")
      .contains(&"  INFO  last job   success — build at 2026-10-11T02:01:30.612Z"),
    "{stdout}"
  );
  assert!(
    block(&stdout, "o/fresh").contains(&"  INFO  last job   no jobs yet"),
    "{stdout}"
  );
  Ok(())
}

#[test]
fn read_only() -> TestResult {
  let home = tempfile::tempdir()?;
  let config = write_registration(home.path(), "o", "r", "http://127.0.0.1:9", false)?;
  let reg_dir = config.parent().ok_or("no parent")?;
  write_lock_body(reg_dir, std::process::id(), &config)?;
  let lock = std::fs::OpenOptions::new()
    .read(true)
    .write(true)
    .open(reg_dir.join(".lock"))?;
  lock.try_lock_exclusive()?;
  let before = tree(home.path())?;

  let (_, stdout, stderr, elapsed) = run(doctor_cmd(home.path())?.arg("--offline"))?;
  assert!(elapsed < Duration::from_secs(30), "{elapsed:?}");
  assert!(
    block(&stdout, "o/r")
      .iter()
      .any(|row| row.starts_with(&format!(
        "  OK    run        active (pid {}",
        std::process::id()
      ))),
    "stdout:\n{stdout}\nstderr:\n{stderr}"
  );
  assert_eq!(tree(home.path())?, before, "doctor changed the home");
  FileExt::unlock(&lock)?;
  Ok(())
}

#[test]
fn service_unit() -> TestResult {
  let home = tempfile::tempdir()?;
  let config = write_registration(home.path(), "o", "r", "http://127.0.0.1:9", false)?;
  write_registration(home.path(), "o", "once", "http://127.0.0.1:9", true)?;

  // Without a unit: an exact WARN with the install command.
  let (_, stdout, _, _) = run(doctor_cmd(home.path())?.arg("--offline"))?;
  let rows = block(&stdout, "o/r");
  assert!(
    rows.contains(&"  WARN  service    not installed"),
    "{stdout}"
  );
  assert!(
    rows.contains(
      &format!(
        "        fix: toolu-runner install-service --config {}",
        config.display()
      )
      .as_str()
    ),
    "{stdout}"
  );
  // An ephemeral registration is never supervised: no service row.
  assert!(
    !block(&stdout, "o/once")
      .iter()
      .any(|row| row.contains(" service ")),
    "{stdout}"
  );

  // The unit the real `install-service --no-activate` writes into this HOME.
  let (code, out, err, _) = run(
    std::process::Command::new(env!("CARGO_BIN_EXE_toolu-runner"))
      .args(["install-service", "--no-activate", "--config"])
      .arg(&config)
      .current_dir(home.path())
      .env("HOME", home.path())
      .env("TOOLU_RUNNER_HOME", home.path()),
  )?;
  assert_eq!(code, Some(0), "stdout:\n{out}\nstderr:\n{err}");
  let (_, stdout, _, _) = run(doctor_cmd(home.path())?.arg("--offline"))?;
  let service = block(&stdout, "o/r")
    .into_iter()
    .find(|row| row.starts_with("  WARN  service    installed ("))
    .ok_or_else(|| format!("no installed-unit row:\n{stdout}"))?;
  assert!(
    service.ends_with("), not active") || service.contains("; could not query "),
    "{service}"
  );
  Ok(())
}

/// The bearer the auth tests hand the binary; synthetic, never a credential.
const TEST_BEARER: &str = "doctor-test-bearer-0f3c9a";

/// The captured real `GET /user` 401 body as an HTTP response.
const USER_401: &str = concat!(
  "HTTP/1.1 401 Unauthorized\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n",
  include_str!("fixtures/doctor/github_user_401.json")
);

#[test]
fn missing_token() -> TestResult {
  let home = tempfile::tempdir()?;
  let config = write_registration(home.path(), "o", "r", "http://127.0.0.1:9", false)?;

  let (code, stdout, stderr, _) = run(doctor_cmd(home.path())?.arg("--offline"))?;
  assert_eq!(code, Some(1), "stdout:\n{stdout}\nstderr:\n{stderr}");
  let token = stdout
    .lines()
    .skip_while(|line| *line != "github auth (127.0.0.1):")
    .skip(1)
    .take(2)
    .collect::<Vec<_>>();
  assert!(
    token
      .first()
      .is_some_and(|row| row.starts_with("  FAIL  token ")),
    "{stdout}"
  );
  assert!(
    token
      .get(1)
      .is_some_and(|fix| fix.starts_with("        fix: toolu-runner login")),
    "{stdout}"
  );

  // The same home with the registration ephemeral: a bearer is not needed.
  std::fs::remove_dir_all(config.parent().ok_or("no parent")?)?;
  write_registration(home.path(), "o", "r", "http://127.0.0.1:9", true)?;
  let (_, stdout, _, _) = run(doctor_cmd(home.path())?.arg("--offline"))?;
  assert!(!stdout.contains("FAIL  token"), "{stdout}");
  Ok(())
}

#[test]
fn rejected_token() -> TestResult {
  let home = tempfile::tempdir()?;
  let api = Recorder::spawn(|head| {
    Some(if head.starts_with("GET /api/v3/user ") {
      USER_401
    } else {
      OK
    })
  })?;
  let base = format!("http://127.0.0.1:{}", api.port());
  write_registration(home.path(), "o", "r", &base, false)?;

  let (code, stdout, stderr, _) = run(
    doctor_cmd(home.path())?
      .args(["--timeout", "5"])
      .env("TOOLU_RUNNER_TOKEN", TEST_BEARER),
  )?;
  assert_eq!(code, Some(1), "stdout:\n{stdout}\nstderr:\n{stderr}");
  assert!(
    stdout.contains(
      "  FAIL  token      TOOLU_RUNNER_TOKEN: rejected by GitHub (HTTP 401: Bad credentials)"
    ),
    "{stdout}"
  );
  let user_request = api
    .heads()
    .into_iter()
    .find(|head| head.starts_with("GET /api/v3/user "))
    .ok_or("doctor never asked GET /api/v3/user")?;
  assert!(
    user_request
      .to_ascii_lowercase()
      .contains(&format!("authorization: bearer {TEST_BEARER}")),
    "{user_request}"
  );
  assert!(!stdout.contains(TEST_BEARER), "{stdout}");
  assert!(!stderr.contains(TEST_BEARER), "{stderr}");
  Ok(())
}

/// The captured real (sanitized) `GET /user` 200 body as an HTTP response.
const USER_200: &str = concat!(
  "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n",
  include_str!("fixtures/doctor/github_user_200.json")
);

/// The rows of the `network (<host>)` section.
fn network_rows<'a>(stdout: &'a str, host: &str) -> Vec<&'a str> {
  stdout
    .lines()
    .skip_while(|line| *line != format!("network ({host}):"))
    .skip(1)
    .take_while(|line| line.starts_with("  "))
    .collect()
}

#[test]
fn healthy() -> TestResult {
  let home = tempfile::tempdir()?;
  let api = Recorder::spawn(|head| {
    Some(if head.starts_with("GET /api/v3/user ") {
      USER_200
    } else {
      OK
    })
  })?;
  let base = format!("http://127.0.0.1:{}", api.port());
  let config = write_registration(home.path(), "o", "r", &base, false)?;
  write_lock_body(
    config.parent().ok_or("no parent")?,
    std::process::id(),
    &config,
  )?;

  let (code, stdout, stderr, _) = run(
    doctor_cmd(home.path())?
      .args(["--timeout", "5"])
      .env("TOOLU_RUNNER_TOKEN", TEST_BEARER),
  )?;
  assert_eq!(code, Some(0), "stdout:\n{stdout}\nstderr:\n{stderr}");
  assert!(
    stdout.contains("  OK    token      TOOLU_RUNNER_TOKEN: valid (login Falconiere)"),
    "{stdout}"
  );
  assert!(
    stdout.contains(&format!(
      "  OK    run        active (pid {}",
      std::process::id()
    )),
    "{stdout}"
  );
  assert!(
    stdout.contains("  WARN  service    not installed"),
    "{stdout}"
  );
  let network = network_rows(&stdout, "127.0.0.1");
  assert!(
    network.iter().any(|row| row.contains("PASS  api ")),
    "{stdout}"
  );
  assert!(stdout.contains("result: 0 failures"), "{stdout}");
  assert!(!stdout.contains(TEST_BEARER), "{stdout}");
  Ok(())
}

#[test]
fn offline_makes_no_requests() -> TestResult {
  let home = tempfile::tempdir()?;
  let peer = Recorder::spawn(|head| Some(github_reply(head)))?;
  let base = format!("http://127.0.0.1:{}", peer.port());
  write_registration(home.path(), "o", "r", &base, false)?;

  let (_, stdout, stderr, _) = run(
    doctor_cmd(home.path())?
      .arg("--offline")
      .env("TOOLU_RUNNER_TOKEN", TEST_BEARER)
      .env(
        "TOOLU_RUNNER_RELEASES_URL",
        format!("{base}/releases/latest"),
      ),
  )?;
  assert!(peer.heads().is_empty(), "requests: {:?}", peer.heads());
  assert!(
    stdout.contains("  SKIP  update     offline"),
    "{stdout}\n{stderr}"
  );
  assert!(
    stdout.contains("  SKIP  token      TOOLU_RUNNER_TOKEN: present, not verified (offline)"),
    "{stdout}"
  );
  assert!(
    network_rows(&stdout, "127.0.0.1").contains(&"  SKIP  endpoints  offline"),
    "{stdout}"
  );
  Ok(())
}

#[test]
fn endpoint_failure() -> TestResult {
  let home = tempfile::tempdir()?;
  let closed = format!("http://127.0.0.1:{}", crate::check_servers::closed_port()?);
  // Ephemeral, so no bearer is needed and only the network can fail.
  write_registration(home.path(), "o", "r", &closed, true)?;

  let (code, stdout, stderr, _) = run(doctor_cmd(home.path())?.args(["--timeout", "5"]))?;
  assert_eq!(code, Some(1), "stdout:\n{stdout}\nstderr:\n{stderr}");
  assert!(
    network_rows(&stdout, "127.0.0.1")
      .iter()
      .any(|row| row.contains("FAIL  api ")),
    "{stdout}"
  );
  Ok(())
}
