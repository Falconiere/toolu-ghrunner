//! #201 pure rules: the installation-token reuse deadline (AC-16) and the
//! runner-group name match. The HTTP calls are exercised against a loopback
//! peer in `toolu-runner/tests/github_app_wire_test.rs`.

use wire::net::github_app::{REUSE_MARGIN_SECS, REUSE_MAX_SECS, reuse_deadline};
use wire::net::runner_groups::{RunnerGroup, pick_group};

/// 2026-10-23T20:00:00Z.
const MINTED: i64 = 1_792_785_600;

#[test]
fn a_fresh_one_hour_token_is_reused_for_fifty_minutes() {
  // GitHub's tokens expire one hour after minting.
  let deadline = reuse_deadline("2026-10-23T21:00:00Z", MINTED);
  assert_eq!(deadline, Some(MINTED + REUSE_MAX_SECS));
  assert_eq!(REUSE_MAX_SECS, 50 * 60);
}

#[test]
fn a_token_expiring_soon_stops_five_minutes_before_expiry() {
  // expires 20 min after minting (an offset RFC 3339 form).
  let deadline = reuse_deadline("2026-10-23T22:20:00+02:00", MINTED);
  assert_eq!(deadline, Some(MINTED + 20 * 60 - REUSE_MARGIN_SECS));
}

#[test]
fn an_already_expired_token_is_never_reused() {
  // The documented example's expires_at (2016) is long past.
  let deadline = reuse_deadline("2016-07-11T22:14:10Z", MINTED).expect("parses");
  assert!(
    deadline <= MINTED,
    "deadline {deadline} must not be after minting"
  );
}

#[test]
fn an_unparseable_expiry_means_single_use() {
  assert_eq!(reuse_deadline("", MINTED), None);
  assert_eq!(reuse_deadline("tomorrow", MINTED), None);
}

fn groups() -> Vec<RunnerGroup> {
  [
    (1, "Default"),
    (2, "octo-runner-group"),
    (3, "Mobile-CI"),
    (4, "mobile-ci"),
  ]
  .into_iter()
  .map(|(id, name)| RunnerGroup {
    id,
    name: name.to_owned(),
    default: id == 1,
    allows_public_repositories: id != 1,
  })
  .collect()
}

#[test]
fn pick_group_prefers_an_exact_name() {
  assert_eq!(pick_group(&groups(), "octo-runner-group"), Some(2));
  assert_eq!(pick_group(&groups(), "mobile-ci"), Some(4));
}

#[test]
fn pick_group_falls_back_to_a_unique_case_insensitive_match() {
  assert_eq!(pick_group(&groups(), "OCTO-runner-GROUP"), Some(2));
}

#[test]
fn pick_group_refuses_an_ambiguous_or_absent_name() {
  assert_eq!(
    pick_group(&groups(), "MOBILE-CI"),
    None,
    "two case-folded matches"
  );
  assert_eq!(pick_group(&groups(), "release"), None);
}

// --- #204 wire helpers: real loopback peer, canned GitHub replies ---

use std::io::{Read as _, Write as _};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Serve `handler(request_head) -> body` on 127.0.0.1 for successive requests.
fn peer(handler: fn(&str) -> String) -> Result<String, Box<dyn std::error::Error>> {
  peer_with_status(move |head| (200, handler(head)))
}

/// Serve `handler(request_head) -> (status, body)` on 127.0.0.1.
fn peer_with_status(
  handler: impl Fn(&str) -> (u16, String) + Send + 'static,
) -> Result<String, Box<dyn std::error::Error>> {
  let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
  let port = listener.local_addr()?.port();
  std::thread::spawn(move || {
    for stream in listener.incoming() {
      let Ok(mut stream) = stream else { continue };
      let mut buf = Vec::new();
      let mut chunk = [0_u8; 4096];
      while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        match stream.read(&mut chunk) {
          Ok(0) | Err(_) => break,
          Ok(n) => buf.extend(chunk.iter().take(n)),
        }
      }
      let head = String::from_utf8_lossy(&buf).into_owned();
      let (status, body) = handler(&head);
      let reply = format!(
        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        body.len(),
        body
      );
      let _ = stream.write_all(reply.as_bytes());
    }
  });
  Ok(format!("http://127.0.0.1:{port}"))
}

fn page_of(head: &str) -> u32 {
  // The `page=` inside `per_page=` must not win: only a `?page=` or
  // `&page=` boundary counts.
  let query = head.split_whitespace().nth(1).unwrap_or_default();
  query
    .split('&')
    .find_map(|part| part.strip_prefix("page="))
    .and_then(|n| n.parse().ok())
    .unwrap_or(1)
}

fn runners_page(head: &str) -> String {
  if page_of(head) == 1 {
    let runners: Vec<String> = (0..100)
      .map(|i| format!(r#"{{"id":{i},"name":"r{i}","status":"offline","busy":false,"labels":[]}}"#))
      .collect();
    format!(r#"{{"total_count":101,"runners":[{}]}}"#, runners.join(","))
  } else {
    r#"{"total_count":101,"runners":[{"id":900,"name":"target","status":"online","busy":true,"labels":[{"id":1,"name":"self-hosted","type":"read-only"},{"id":2,"name":"gpu","type":"custom"}]}]}"#.to_owned()
  }
}

#[tokio::test]
async fn find_runner_paginates_and_maps_labels() -> TestResult {
  let base = peer(runners_page)?;
  let client = reqwest::Client::new();
  let found = wire::net::register::find_runner(
    &client,
    &format!("{base}/acme"),
    "token",
    "target",
    std::time::Duration::from_secs(5),
  )
  .await?
  .ok_or("runner present")?;
  assert_eq!(found.id, 900);
  assert_eq!(found.status, "online");
  assert!(found.busy);
  assert_eq!(
    found.labels,
    vec!["self-hosted".to_owned(), "gpu".to_owned()]
  );
  Ok(())
}

#[tokio::test]
async fn find_runner_absent_returns_none() -> TestResult {
  let base = peer(|_| r#"{"total_count":0,"runners":[]}"#.to_owned())?;
  let client = reqwest::Client::new();
  let found = wire::net::register::find_runner(
    &client,
    &format!("{base}/acme"),
    "token",
    "ghost",
    std::time::Duration::from_secs(5),
  )
  .await?;
  assert_eq!(found, None);
  Ok(())
}

fn repos_page(head: &str) -> String {
  if head.contains("/installation/repositories") {
    if page_of(head) == 1 {
      let repos: Vec<String> = (0..100)
        .map(|i| format!(r#"{{"full_name":"acme/r{i}"}}"#))
        .collect();
      format!(
        r#"{{"total_count":102,"repositories":[{}]}}"#,
        repos.join(",")
      )
    } else {
      r#"{"total_count":102,"repositories":[{"full_name":"acme/last"},{"full_name":"acme/final"}]}"#
        .to_owned()
    }
  } else {
    r#"[{"full_name":"octo/web"},{"full_name":"octo/api"}]"#.to_owned()
  }
}

#[tokio::test]
async fn repo_lists_paginate_to_exhaustion() -> TestResult {
  let base = peer(repos_page)?;
  let client = reqwest::Client::new();
  let installed = wire::net::github_app::installation_repositories(
    &client,
    &base,
    "token",
    std::time::Duration::from_secs(5),
  )
  .await?;
  assert_eq!(installed.len(), 102);
  assert_eq!(
    installed.last().map(|repo| repo.full_name.as_str()),
    Some("acme/final")
  );
  let mine = wire::net::github_app::user_repositories(
    &client,
    &base,
    "token",
    std::time::Duration::from_secs(5),
  )
  .await?;
  assert_eq!(mine.len(), 2);
  assert_eq!(
    mine.first().map(|repo| repo.full_name.as_str()),
    Some("octo/web")
  );
  Ok(())
}

#[tokio::test]
async fn runner_groups_parse_default_flags() -> TestResult {
  let base = peer(|_| {
    r#"{"total_count":2,"runner_groups":[{"id":1,"name":"Default","default":true,"allows_public_repositories":false},{"id":7,"name":"ci","default":false,"allows_public_repositories":true}]}"#.to_owned()
  })?;
  let client = reqwest::Client::new();
  let groups = wire::net::runner_groups::list_runner_groups(
    &client,
    &base,
    "acme",
    "token",
    std::time::Duration::from_secs(5),
  )
  .await?;
  let first = groups.first().ok_or("first group")?;
  assert!(first.default);
  assert!(!first.allows_public_repositories);
  assert_eq!(groups.get(1).map(|group| group.id), Some(7));
  Ok(())
}

#[tokio::test]
async fn find_runner_non_json_401_is_auth_not_network() -> TestResult {
  let base = peer_with_status(|_| (401, "not json".to_owned()))?;
  let client = reqwest::Client::new();
  let err = wire::net::register::find_runner(
    &client,
    &format!("{base}/acme"),
    "token",
    "target",
    std::time::Duration::from_secs(5),
  )
  .await
  .err()
  .ok_or("expected an error")?;
  assert!(
    matches!(err, shared::RunnerError::Auth(_)),
    "401 with a non-JSON body must be Auth, got {err:?}"
  );
  Ok(())
}
