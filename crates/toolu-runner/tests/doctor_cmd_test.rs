//! Shell-out tests for `toolu-runner doctor`: the compiled binary run against
//! real registrations (the committed GHES JIT fixture), real processes, real
//! files and loopback peers serving bodies captured from the live GitHub API.
//!
//! Rows are asserted whole: the level column, the padded name, the detail and
//! the `fix:` line beneath. Only text the OS or a dependency owns (an errno,
//! a measured millisecond count, free disk space) is matched by its stable
//! frame instead.
//!
//! The shared fixtures and row helpers live in `helpers/doctor_env.rs`. The
//! auth, release, flag, service and network/summary tests live in the sibling
//! `doctor_*_test.rs` files; the ones here are the plan's per-step checks.

use std::os::unix::fs::PermissionsExt as _;
use std::time::Duration;

use fs2::FileExt;

use crate::check_servers::{Recorder, closed_port, run};
use crate::doctor_env::{
  ENV_REJECTED_FIX, REREGISTER, STORED_BEARER, TEST_BEARER, USER_401, VERSION, block, dead_pid,
  doctor_cmd, fix, heading, healthy_reply, implied_summary, install_fix, jit_ok, last_line, report,
  row, section, service_install_fix, snapshot, store_login, user_authorization, user_peer,
  write_lock_body, write_registration, write_registration_with,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn empty_home() -> TestResult {
  let home = tempfile::tempdir()?;
  let (code, stdout, stderr, _) = run(doctor_cmd(home.path())?.arg("--offline"))?;

  assert_eq!(
    section(&stdout, "registrations"),
    [
      row("INFO", "registrations", "none"),
      fix("toolu-runner register --url https://github.com/<owner>/<repo>"),
    ],
    "{stdout}"
  );
  assert_eq!(
    section(&stdout, "install"),
    [
      row(
        "OK",
        "version",
        &format!(
          "toolu-runner {VERSION} (GitHub runner compatibility {})",
          protocol::runner_version::COMPATIBILITY_VERSION
        )
      ),
      row("SKIP", "update", "offline"),
    ],
    "{stdout}"
  );
  assert_eq!(
    section(&stdout, "github auth (github.com)"),
    [
      row(
        "INFO",
        "token",
        "none (only needed for persistent token registrations)"
      ),
      fix("toolu-runner login"),
    ],
    "{stdout}"
  );
  assert_eq!(
    section(&stdout, "network (github.com)"),
    [row("SKIP", "endpoints", "offline")],
    "{stdout}"
  );
  let disk = format!(" GiB free at {}", home.path().display());
  let host = section(&stdout, "host");
  let disk_rows: Vec<(&str, &str)> = ["OK", "WARN", "FAIL"]
    .into_iter()
    .flat_map(|level| {
      let prefix = row(level, "disk", "");
      host
        .iter()
        .filter_map(move |line| line.strip_prefix(&prefix).map(|detail| (level, detail)))
        .collect::<Vec<_>>()
    })
    .collect();
  let (level, detail) = *disk_rows.first().ok_or("doctor omitted the disk row")?;
  assert_eq!(disk_rows.len(), 1, "{stdout}");
  assert!(
    detail
      .strip_suffix(&disk)
      .is_some_and(|gib| gib.parse::<f64>().is_ok()),
    "{stdout}"
  );
  if level != "OK" {
    let disk_fix = fix(&format!(
      "free space on the volume holding {}",
      home.path().display()
    ));
    assert!(host.contains(&disk_fix.as_str()), "{stdout}");
  }
  let expected_code = i32::from(level == "FAIL");
  assert_eq!(code, Some(expected_code), "{}", report(&stdout, &stderr));
  assert_eq!(
    last_line(&stdout),
    implied_summary(&stdout, expected_code),
    "{stdout}"
  );
  Ok(())
}

#[test]
fn update_check() -> TestResult {
  let home = tempfile::tempdir()?;
  let peer = Recorder::spawn(|head| Some(healthy_reply(head)))?;
  let base = format!("http://127.0.0.1:{}", peer.port());
  // Ephemeral: no bearer is needed, so the token section cannot fail.
  write_registration(home.path(), "o", "r", &base, true)?;

  let (code, stdout, stderr, _) = run(doctor_cmd(home.path())?.args(["--timeout", "5"]).env(
    "TOOLU_RUNNER_RELEASES_URL",
    format!("{base}/releases/latest"),
  ))?;
  // Build versions only grow: the captured v0.11.1 is the build or older.
  let install = section(&stdout, "install");
  assert!(
    install.contains(&row("OK", "update", "up to date (latest v0.11.1)").as_str())
      || install.contains(&row("OK", "update", "newer than the latest release (v0.11.1)").as_str()),
    "{}",
    report(&stdout, &stderr)
  );
  assert!(
    peer
      .request_lines()
      .contains(&"GET /releases/latest HTTP/1.1".to_owned()),
    "{:?}",
    peer.request_lines()
  );
  assert_eq!(code, Some(0), "{}", report(&stdout, &stderr));

  // The default (closed port) release URL: SKIP, and the exit is unaffected.
  let closed = closed_port()?;
  let (unreachable_code, unreachable, _, _) =
    run(doctor_cmd(home.path())?.args(["--timeout", "5"]).env(
      "TOOLU_RUNNER_RELEASES_URL",
      format!("http://127.0.0.1:{closed}/releases/latest"),
    ))?;
  let skip = section(&unreachable, "install")
    .into_iter()
    .find(|line| line.starts_with("  SKIP  update "))
    .ok_or_else(|| format!("no SKIP update row:\n{unreachable}"))?;
  // The errno spelling is the OS's: match the row up to it.
  let frame = row(
    "SKIP",
    "update",
    "could not check latest release: connect: error sending request: client error \
     (Connect): tcp connect error: Connection refused (os error ",
  );
  assert!(
    skip.starts_with(&frame) && skip.ends_with(')'),
    "{skip}\nexpected to start with:\n{frame}"
  );
  assert_eq!(unreachable_code, code, "{unreachable}");
  Ok(())
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
  let missing_creds = missing.with_file_name("credentials.json");
  std::fs::remove_file(&missing_creds)?;

  let (code, stdout, stderr, _) = run(doctor_cmd(home.path())?.arg("--offline"))?;
  assert_eq!(code, Some(1), "{}", report(&stdout, &stderr));

  // A config that does not parse is the only row of its block, and the
  // parse error's source excerpt (which could quote a secret) is dropped.
  let bad_rows = block(&stdout, "o/bad");
  assert_eq!(
    bad_rows,
    [
      row(
        "FAIL",
        "config",
        &format!(
          "cannot load {0}: config error: parse {0}: TOML parse error at line 1, column 15; \
           invalid array; expected `]`",
          bad.display()
        )
      ),
      fix(REREGISTER),
    ],
    "{stdout}"
  );
  assert!(!stdout.contains("runner_url = [unterminated"), "{stdout}");

  // A loose-mode credentials file warns and every other row is still reported.
  assert_eq!(
    block(&stdout, "o/loose"),
    [
      row("OK", "config", &loose.display().to_string()),
      row("WARN", "creds", "mode 0644: readable beyond the owner"),
      fix(&format!("chmod 600 {}", loose_creds.display())),
      jit_ok(&loose)?,
      row("WARN", "run", "not running"),
      fix(&format!("toolu-runner run --config {}", loose.display())),
      row("INFO", "last job", "no jobs yet"),
    ],
    "{stdout}"
  );

  assert_eq!(
    block(&stdout, "o/nocreds"),
    [
      row("OK", "config", &missing.display().to_string()),
      row(
        "FAIL",
        "creds",
        &format!("missing {}", missing_creds.display())
      ),
      fix(REREGISTER),
      jit_ok(&missing)?,
      row("WARN", "run", "not running"),
      fix(&format!("toolu-runner run --config {}", missing.display())),
      row("INFO", "last job", "no jobs yet"),
    ],
    "{stdout}"
  );
  assert_eq!(last_line(&stdout), implied_summary(&stdout, 1), "{stdout}");
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
  assert_eq!(code, Some(1), "{}", report(&stdout, &stderr));
  assert_eq!(
    block(&stdout, "o/r"),
    [
      row("OK", "config", &config.display().to_string()),
      row("OK", "creds", "present (0600)"),
      jit_ok(&config)?,
      row("FAIL", "run", "not running"),
      install_fix(&config),
      row("WARN", "service", "not installed"),
      service_install_fix(),
      row(
        "WARN",
        "pending",
        "removal pending: `run` exits at its next job boundary"
      ),
      fix(&format!(
        "toolu-runner remove --config {}",
        config.display()
      )),
      row("INFO", "last job", "no jobs yet"),
    ],
    "{stdout}"
  );
  assert!(
    section(&stdout, "registrations").contains(&heading("o/r", "persistent").as_str()),
    "{stdout}"
  );

  // A lock body that is not JSON is "not running" and says why.
  std::fs::write(reg_dir.join(".lock"), "not json")?;
  let (_, stdout, _, _) = run(doctor_cmd(home.path())?.arg("--offline"))?;
  let unreadable = block(&stdout, "o/r")
    .into_iter()
    .find(|line| line.starts_with("  FAIL  run "))
    .ok_or_else(|| format!("no FAIL run row:\n{stdout}"))?;
  assert_eq!(
    unreadable,
    row(
      "FAIL",
      "run",
      "lock body unreadable (expected ident at line 1 column 2); treated as not running"
    ),
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
    block(&stdout, "o/r").contains(
      &row(
        "INFO",
        "last job",
        "success — build at 2026-10-11T02:01:30.612Z"
      )
      .as_str()
    ),
    "{stdout}"
  );
  assert!(
    block(&stdout, "o/fresh").contains(&row("INFO", "last job", "no jobs yet").as_str()),
    "{stdout}"
  );
  Ok(())
}

#[test]
fn read_only() -> TestResult {
  let home = tempfile::tempdir()?;
  let peer = Recorder::spawn(|head| Some(healthy_reply(head)))?;
  let base = format!("http://127.0.0.1:{}", peer.port());
  let config = write_registration(home.path(), "o", "r", &base, false)?;
  let reg_dir = config.parent().ok_or("no parent")?;
  let since = write_lock_body(reg_dir, std::process::id(), &config)?;
  store_login(home.path(), "127.0.0.1", STORED_BEARER)?;
  std::fs::write(reg_dir.join(".pending_remove"), "")?;
  let lock = std::fs::OpenOptions::new()
    .read(true)
    .write(true)
    .open(reg_dir.join(".lock"))?;
  lock.try_lock_exclusive()?;
  let before = snapshot(home.path())?;
  let active = row(
    "OK",
    "run",
    &format!("active (pid {}, since {since})", std::process::id()),
  );

  // Offline, with another process holding the registration's lock.
  let (_, stdout, stderr, elapsed) = run(doctor_cmd(home.path())?.arg("--offline"))?;
  assert!(elapsed < Duration::from_secs(30), "{elapsed:?}");
  assert!(
    block(&stdout, "o/r").contains(&active.as_str()),
    "{}",
    report(&stdout, &stderr)
  );
  assert_eq!(
    snapshot(home.path())?,
    before,
    "an offline doctor changed the home"
  );

  // Online: the bearer, the release document and every endpoint are fetched,
  // and the home is still byte-, mode- and mtime-identical.
  let release = format!("http://127.0.0.1:{}/releases/latest", peer.port());
  let (code, stdout, stderr, _) = run(
    doctor_cmd(home.path())?
      .args(["--timeout", "5"])
      .env("TOOLU_RUNNER_RELEASES_URL", release),
  )?;
  let requested = peer.request_lines();
  for path in ["/api/v3/user", "/releases/latest", "/api/v3"] {
    assert!(
      requested.contains(&format!("GET {path} HTTP/1.1")),
      "doctor never asked {path}: {requested:?}"
    );
  }
  assert!(
    block(&stdout, "o/r").contains(&active.as_str()),
    "{}",
    report(&stdout, &stderr)
  );
  assert_eq!(code, Some(0), "{}", report(&stdout, &stderr));
  assert_eq!(
    snapshot(home.path())?,
    before,
    "an online doctor changed the home"
  );
  FileExt::unlock(&lock)?;
  Ok(())
}

#[test]
fn missing_token() -> TestResult {
  let home = tempfile::tempdir()?;
  let config = write_registration(home.path(), "o", "r", "http://127.0.0.1:9", false)?;

  let (code, stdout, stderr, _) = run(doctor_cmd(home.path())?.arg("--offline"))?;
  assert_eq!(code, Some(1), "{}", report(&stdout, &stderr));
  assert_eq!(
    section(&stdout, "github auth (127.0.0.1)"),
    [
      row(
        "FAIL",
        "token",
        "none: a persistent registration cannot re-mint after its next job"
      ),
      fix("toolu-runner login 127.0.0.1"),
    ],
    "{stdout}"
  );

  // The same home with the registration ephemeral: a bearer is not needed.
  std::fs::remove_dir_all(config.parent().ok_or("no parent")?)?;
  write_registration(home.path(), "o", "r", "http://127.0.0.1:9", true)?;
  let (_, stdout, _, _) = run(doctor_cmd(home.path())?.arg("--offline"))?;
  assert_eq!(
    section(&stdout, "github auth (127.0.0.1)"),
    [
      row(
        "INFO",
        "token",
        "none (only needed for persistent token registrations)"
      ),
      fix("toolu-runner login 127.0.0.1"),
    ],
    "{stdout}"
  );
  Ok(())
}

/// #201 AC-15: a persistent registration that re-mints with a GitHub App
/// needs no personal token, so its missing login is not a failure.
#[test]
fn missing_token_is_not_a_failure_for_an_app_registration() -> TestResult {
  let home = tempfile::tempdir()?;
  write_registration_with(home.path(), "o", "r", "http://127.0.0.1:9", false, |cfg| {
    cfg.auth = config::registration_auth::AuthSection::App {
      app_file: "/nonexistent/github-app.json".to_owned(),
      installation_id: 1,
    };
  })?;

  // Only the auth section is under test: the registration is persistent
  // and not running, which `doctor` FAILs on its own `run` row.
  let (_, stdout, _, _) = run(doctor_cmd(home.path())?.arg("--offline"))?;
  assert_eq!(
    section(&stdout, "github auth (127.0.0.1)"),
    [
      row(
        "INFO",
        "token",
        "none (only needed for persistent token registrations)"
      ),
      fix("toolu-runner login 127.0.0.1"),
    ],
    "{stdout}"
  );
  Ok(())
}

#[test]
fn rejected_token() -> TestResult {
  let home = tempfile::tempdir()?;
  let api = user_peer!(USER_401)?;
  let base = format!("http://127.0.0.1:{}", api.port());
  write_registration(home.path(), "o", "r", &base, false)?;

  let (code, stdout, stderr, _) = run(
    doctor_cmd(home.path())?
      .args(["--timeout", "5"])
      .env("TOOLU_RUNNER_TOKEN", TEST_BEARER),
  )?;
  assert_eq!(code, Some(1), "{}", report(&stdout, &stderr));
  assert_eq!(
    section(&stdout, "github auth (127.0.0.1)"),
    [
      row(
        "FAIL",
        "token",
        "TOOLU_RUNNER_TOKEN: rejected by GitHub (HTTP 401: Bad credentials)"
      ),
      fix(ENV_REJECTED_FIX),
    ],
    "{stdout}"
  );
  assert_eq!(
    user_authorization(&api)?,
    format!("authorization: bearer {TEST_BEARER}")
  );
  assert!(!stdout.contains(TEST_BEARER), "{stdout}");
  assert!(!stderr.contains(TEST_BEARER), "{stderr}");
  Ok(())
}

/// #201 F5: an App registration gets an `app` row: OK naming the App when its
/// file loads, FAIL with the fix when it is gone.
#[test]
fn an_app_registration_reports_its_app_file() -> TestResult {
  let home = tempfile::tempdir()?;
  let app_file = crate::github_app_fixtures::write_app(home.path(), "127.0.0.1")?;
  let app_file_text = app_file.to_string_lossy().into_owned();
  let auth = config::registration_auth::AuthSection::App {
    app_file: app_file_text.clone(),
    installation_id: 1,
  };
  write_registration_with(home.path(), "o", "r", "http://127.0.0.1:9", false, |cfg| {
    cfg.auth = auth;
  })?;

  let (_, stdout, _, _) = run(doctor_cmd(home.path())?.arg("--offline"))?;
  let ok = row(
    "OK",
    "app",
    "GitHub App 'toolu-runner-test-app' (App 424242, installation 1)",
  );
  assert!(block(&stdout, "o/r").contains(&ok.as_str()), "{stdout}");

  std::fs::remove_file(&app_file)?;
  let (code, stdout, stderr, _) = run(doctor_cmd(home.path())?.arg("--offline"))?;
  let fail = row(
    "FAIL",
    "app",
    &format!(
      "config error: no GitHub App saved at {app_file_text} — run `toolu-runner create-app` first"
    ),
  );
  let restore = fix(&format!(
    "restore {app_file_text}, or re-register: toolu-runner register --app --replace --url \
     http://127.0.0.1:9/o/r"
  ));
  let registration = block(&stdout, "o/r");
  assert!(registration.contains(&fail.as_str()), "{stdout}");
  assert!(registration.contains(&restore.as_str()), "{stdout}");
  assert_eq!(code, Some(1), "{}", report(&stdout, &stderr));
  Ok(())
}

/// An ephemeral App registration never re-mints, so it gets no `app` row
/// even when its App file is gone.
#[test]
fn an_ephemeral_app_registration_has_no_app_row() -> TestResult {
  let home = tempfile::tempdir()?;
  write_registration_with(home.path(), "o", "r", "http://127.0.0.1:9", true, |cfg| {
    cfg.auth = config::registration_auth::AuthSection::App {
      app_file: "/nonexistent/github-app.json".to_owned(),
      installation_id: 1,
    };
  })?;
  let (_, stdout, _, _) = run(doctor_cmd(home.path())?.arg("--offline"))?;
  let registration = block(&stdout, "o/r");
  assert!(
    !registration.iter().any(|line| line.contains(" app ")),
    "{stdout}"
  );
  Ok(())
}
