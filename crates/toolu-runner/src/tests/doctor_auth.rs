//! Tests for the `doctor` GitHub auth rows: API-base derivation (the same
//! rule `register` uses), host grouping, and the verdict on the captured real
//! `GET /user` 200 and 401 bodies.

use super::{api_base, verdict_row};
use crate::doctor_report::Level;

const USER_200: &str = include_str!("../../tests/fixtures/doctor/github_user_200.json");
const USER_401: &str = include_str!("../../tests/fixtures/doctor/github_user_401.json");

#[test]
fn api_base_follows_registers_rule() {
  assert_eq!(
    api_base("https://github.com/o/r"),
    Some(("github.com".to_owned(), "https://api.github.com".to_owned()))
  );
  assert_eq!(
    api_base("https://ghes.example.com/o/r"),
    Some((
      "ghes.example.com".to_owned(),
      "https://ghes.example.com/api/v3".to_owned()
    ))
  );
  assert_eq!(
    api_base("http://127.0.0.1:8080/o/r"),
    Some((
      "127.0.0.1".to_owned(),
      "http://127.0.0.1:8080/api/v3".to_owned()
    ))
  );
  assert_eq!(api_base("not a url"), None);
}

#[test]
fn a_real_200_names_the_login_and_scopes() {
  let row = verdict_row(
    "TOOLU_RUNNER_TOKEN",
    200,
    USER_200,
    Some("repo, admin:org"),
    "github.com",
  );
  assert_eq!(row.level, Level::Ok);
  assert_eq!(
    row.detail,
    "TOOLU_RUNNER_TOKEN: valid (login Falconiere; scopes repo, admin:org)"
  );
  let no_scopes = verdict_row(
    "stored login (0600 file)",
    200,
    USER_200,
    None,
    "github.com",
  );
  assert_eq!(
    no_scopes.detail,
    "stored login (0600 file): valid (login Falconiere)"
  );
}

#[test]
fn a_real_401_fails_with_login_and_other_statuses_warn() {
  let rejected = verdict_row(
    "stored login (0600 file)",
    401,
    USER_401,
    None,
    "github.com",
  );
  assert_eq!(rejected.level, Level::Fail);
  assert_eq!(
    rejected.detail,
    "stored login (0600 file): rejected by GitHub (HTTP 401: Bad credentials)"
  );
  assert_eq!(rejected.fix.as_deref(), Some("toolu-runner login"));

  let ghes = verdict_row(
    "stored login (0600 file)",
    401,
    USER_401,
    None,
    "ghes.example.com",
  );
  assert_eq!(
    ghes.fix.as_deref(),
    Some("toolu-runner login ghes.example.com")
  );

  for status in [403, 500, 502] {
    let row = verdict_row("TOOLU_RUNNER_TOKEN", status, "", None, "github.com");
    assert_eq!(row.level, Level::Warn, "{status}");
    assert_eq!(
      row.detail,
      format!("TOOLU_RUNNER_TOKEN: could not verify (HTTP {status})")
    );
  }
}
