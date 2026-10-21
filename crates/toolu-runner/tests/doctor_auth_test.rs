//! Shell-out tests for the `github auth` section of `toolu-runner doctor`: the
//! bearer's source and precedence (env token over stored login), the real
//! `GET /user` verdicts (valid, scopes, 401, unverifiable), the unreadable
//! token store, and that no bearer is ever printed. Peers are loopback servers
//! replaying bodies captured from the live GitHub API.

use std::os::unix::fs::PermissionsExt as _;

use crate::check_servers::run;
use crate::doctor_env::{
  ENV_REJECTED_FIX, HTTP_500, STORED_BEARER, TEST_BEARER, USER_200, USER_200_EMPTY_SCOPES,
  USER_200_SCOPES, USER_401, USER_401_ECHOES_ENV, USER_401_ECHOES_STORED, doctor_cmd, fix,
  implied_summary, last_line, report, row, section, store_login, user_authorization, user_peer,
  write_lock_body, write_registration,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn rejected_token_is_a_warning_without_a_persistent_registration() -> TestResult {
  let home = tempfile::tempdir()?;
  let api = user_peer!(USER_401)?;
  let base = format!("http://127.0.0.1:{}", api.port());
  // Ephemeral only: nothing re-mints with the bearer, so a rejection is news
  // but not a failure.
  write_registration(home.path(), "o", "r", &base, true)?;

  let (code, stdout, stderr, _) = run(
    doctor_cmd(home.path())?
      .args(["--timeout", "5"])
      .env("TOOLU_RUNNER_TOKEN", TEST_BEARER),
  )?;
  assert_eq!(
    section(&stdout, "github auth (127.0.0.1)"),
    [
      row(
        "WARN",
        "token",
        "TOOLU_RUNNER_TOKEN: rejected by GitHub (HTTP 401: Bad credentials)"
      ),
      fix(ENV_REJECTED_FIX),
    ],
    "{stdout}"
  );
  assert_eq!(code, Some(0), "{}", report(&stdout, &stderr));
  assert_eq!(last_line(&stdout), implied_summary(&stdout, 0), "{stdout}");
  Ok(())
}

#[test]
fn rejected_stored_login_asks_only_for_login() -> TestResult {
  let home = tempfile::tempdir()?;
  assert!(
    USER_401_ECHOES_STORED.contains(STORED_BEARER),
    "vacuous reply"
  );
  let api = user_peer!(USER_401_ECHOES_STORED)?;
  let base = format!("http://127.0.0.1:{}", api.port());
  write_registration(home.path(), "o", "r", &base, false)?;
  store_login(home.path(), "127.0.0.1", STORED_BEARER)?;

  let (code, stdout, stderr, _) = run(doctor_cmd(home.path())?.args(["--timeout", "5"]))?;
  assert_eq!(code, Some(1), "{}", report(&stdout, &stderr));
  // A stored login is replaced by `login`; the env-token advice would be
  // wrong here. The echoed token is masked.
  assert_eq!(
    section(&stdout, "github auth (127.0.0.1)"),
    [
      row(
        "FAIL",
        "token",
        "stored login (0600 file): rejected by GitHub (HTTP 401: Bad credentials for token ***)"
      ),
      fix("toolu-runner login 127.0.0.1"),
    ],
    "{stdout}"
  );
  assert_eq!(
    user_authorization(&api)?,
    format!("authorization: bearer {STORED_BEARER}")
  );
  assert!(!stdout.contains(STORED_BEARER), "{stdout}");
  assert!(!stderr.contains(STORED_BEARER), "{stderr}");
  Ok(())
}

#[test]
fn unverifiable_token_is_a_warning() -> TestResult {
  let home = tempfile::tempdir()?;
  let api = user_peer!(HTTP_500)?;
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
  // GitHub having a bad moment says nothing about the token: no fix line,
  // and no failure even though a persistent registration needs the bearer.
  assert_eq!(
    section(&stdout, "github auth (127.0.0.1)"),
    [row(
      "WARN",
      "token",
      "TOOLU_RUNNER_TOKEN: could not verify (HTTP 500)"
    )],
    "{stdout}"
  );
  assert_eq!(code, Some(0), "{}", report(&stdout, &stderr));
  Ok(())
}

#[test]
fn echoed_bearer_is_masked() -> TestResult {
  let home = tempfile::tempdir()?;
  // A misbehaving proxy echoes the Authorization bearer into the 401 body.
  assert!(USER_401_ECHOES_ENV.contains(TEST_BEARER), "vacuous reply");
  let api = user_peer!(USER_401_ECHOES_ENV)?;
  let base = format!("http://127.0.0.1:{}", api.port());
  write_registration(home.path(), "o", "r", &base, false)?;

  let (code, stdout, stderr, _) = run(
    doctor_cmd(home.path())?
      .args(["--timeout", "5"])
      .env("TOOLU_RUNNER_TOKEN", TEST_BEARER),
  )?;
  assert_eq!(code, Some(1), "{}", report(&stdout, &stderr));
  // The peer really did receive the bearer, so the echo is real.
  assert_eq!(
    user_authorization(&api)?,
    format!("authorization: bearer {TEST_BEARER}")
  );
  assert_eq!(
    section(&stdout, "github auth (127.0.0.1)"),
    [
      row(
        "FAIL",
        "token",
        "TOOLU_RUNNER_TOKEN: rejected by GitHub (HTTP 401: Bad credentials for token ***)"
      ),
      fix(ENV_REJECTED_FIX),
    ],
    "{stdout}"
  );
  assert!(!stdout.contains(TEST_BEARER), "{stdout}");
  assert!(!stderr.contains(TEST_BEARER), "{stderr}");
  Ok(())
}

#[test]
fn token_scopes_are_reported() -> TestResult {
  let home = tempfile::tempdir()?;
  let api = user_peer!(USER_200_SCOPES)?;
  write_registration(
    home.path(),
    "o",
    "r",
    &format!("http://127.0.0.1:{}", api.port()),
    true,
  )?;

  let (code, stdout, stderr, _) = run(
    doctor_cmd(home.path())?
      .args(["--timeout", "5"])
      .env("TOOLU_RUNNER_TOKEN", TEST_BEARER),
  )?;
  assert_eq!(code, Some(0), "{}", report(&stdout, &stderr));
  assert_eq!(
    section(&stdout, "github auth (127.0.0.1)"),
    [row(
      "OK",
      "token",
      "TOOLU_RUNNER_TOKEN: valid (login Falconiere; scopes repo, workflow)"
    )],
    "{stdout}"
  );

  // A token without scopes (fine-grained, app) prints none.
  let bare = user_peer!(USER_200_EMPTY_SCOPES)?;
  let bare_home = tempfile::tempdir()?;
  write_registration(
    bare_home.path(),
    "o",
    "r",
    &format!("http://127.0.0.1:{}", bare.port()),
    true,
  )?;
  let (_, stdout, _, _) = run(
    doctor_cmd(bare_home.path())?
      .args(["--timeout", "5"])
      .env("TOOLU_RUNNER_TOKEN", TEST_BEARER),
  )?;
  assert_eq!(
    section(&stdout, "github auth (127.0.0.1)"),
    [row(
      "OK",
      "token",
      "TOOLU_RUNNER_TOKEN: valid (login Falconiere)"
    )],
    "{stdout}"
  );
  Ok(())
}

#[test]
fn stored_login_is_the_bearer_without_an_env_token() -> TestResult {
  let home = tempfile::tempdir()?;
  let api = user_peer!(USER_200)?;
  let base = format!("http://127.0.0.1:{}", api.port());
  write_registration(home.path(), "o", "r", &base, true)?;
  let token_file = store_login(home.path(), "127.0.0.1", STORED_BEARER)?;
  assert_eq!(
    std::fs::metadata(&token_file)?.permissions().mode() & 0o777,
    0o600
  );

  let (code, stdout, stderr, _) = run(doctor_cmd(home.path())?.args(["--timeout", "5"]))?;
  assert_eq!(code, Some(0), "{}", report(&stdout, &stderr));
  assert_eq!(
    section(&stdout, "github auth (127.0.0.1)"),
    [row(
      "OK",
      "token",
      "stored login (0600 file): valid (login Falconiere)"
    )],
    "{stdout}"
  );
  assert_eq!(
    user_authorization(&api)?,
    format!("authorization: bearer {STORED_BEARER}")
  );
  assert!(!stdout.contains(STORED_BEARER), "{stdout}");
  assert!(!stderr.contains(STORED_BEARER), "{stderr}");
  Ok(())
}

#[test]
fn env_token_outranks_the_stored_login() -> TestResult {
  let home = tempfile::tempdir()?;
  let api = user_peer!(USER_200)?;
  let base = format!("http://127.0.0.1:{}", api.port());
  write_registration(home.path(), "o", "r", &base, true)?;
  store_login(home.path(), "127.0.0.1", STORED_BEARER)?;

  let (code, stdout, stderr, _) = run(
    doctor_cmd(home.path())?
      .args(["--timeout", "5"])
      .env("TOOLU_RUNNER_TOKEN", TEST_BEARER),
  )?;
  assert_eq!(code, Some(0), "{}", report(&stdout, &stderr));
  assert_eq!(
    section(&stdout, "github auth (127.0.0.1)"),
    [row(
      "OK",
      "token",
      "TOOLU_RUNNER_TOKEN: valid (login Falconiere)"
    )],
    "{stdout}"
  );
  // Exactly one credentialed request, with the env token.
  let users: Vec<_> = api
    .request_lines()
    .into_iter()
    .filter(|line| line.starts_with("GET /api/v3/user "))
    .collect();
  assert_eq!(users, ["GET /api/v3/user HTTP/1.1"]);
  assert_eq!(
    user_authorization(&api)?,
    format!("authorization: bearer {TEST_BEARER}")
  );
  for text in [&stdout, &stderr] {
    assert!(!text.contains(TEST_BEARER) && !text.contains(STORED_BEARER));
  }
  Ok(())
}

#[test]
fn unreadable_token_store() -> TestResult {
  let home = tempfile::tempdir()?;
  let persistent = write_registration(home.path(), "o", "r", "http://127.0.0.1:9", false)?;
  let token_file = home.path().join("token-127.0.0.1.json");
  std::fs::write(&token_file, [0xff_u8, 0xfe, 0x00, 0x01])?;
  write_lock_body(
    persistent.parent().ok_or("no parent")?,
    std::process::id(),
    &persistent,
  )?;

  let (code, stdout, stderr, _) = run(doctor_cmd(home.path())?.arg("--offline"))?;
  assert_eq!(code, Some(1), "{}", report(&stdout, &stderr));
  assert_eq!(
    section(&stdout, "github auth (127.0.0.1)"),
    [
      row(
        "FAIL",
        "token",
        "token store unreadable: JSON error: expected value at line 1 column 1"
      ),
      fix("toolu-runner login 127.0.0.1"),
    ],
    "{stdout}"
  );

  // Without a persistent registration nothing re-mints: a warning.
  std::fs::remove_dir_all(persistent.parent().ok_or("no parent")?)?;
  write_registration(home.path(), "o", "r", "http://127.0.0.1:9", true)?;
  let (_, stdout, _, _) = run(doctor_cmd(home.path())?.arg("--offline"))?;
  assert_eq!(
    section(&stdout, "github auth (127.0.0.1)"),
    [
      row(
        "WARN",
        "token",
        "token store unreadable: JSON error: expected value at line 1 column 1"
      ),
      fix("toolu-runner login 127.0.0.1"),
    ],
    "{stdout}"
  );
  Ok(())
}
