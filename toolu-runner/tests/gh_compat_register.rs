//! Real-data tests for the live JIT register flow (`net::register`).
//!
//! Covers the github.com `generate-jitconfig` contract three ways, none
//! needing a real token:
//!  - request building (`build_request`): URL, method-agnostic body shape;
//!  - response parsing (`parse_response`): a committed real-shaped JSON
//!    fixture whose `encoded_jit_config` is a genuine parseable 3-blob
//!    envelope (reused from `fixtures/jit_config_github_com.json`);
//!  - the all-or-nothing send (`register_jit`) over a local wiremock stub:
//!    success parses; a non-2xx surfaces GitHub's body and yields `Err`.
//!
//! Live end-to-end validation (AC-1) is gated on `TOOLU_RUNNER_LIVE_TOKEN`
//! and lives in `tests/live/` — it is NOT exercised here.

use serde_json::Value;
use shared::RunnerError;
use toolu_runner::net::{RegisterParams, build_request, parse_response, register_jit};
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Committed real-shaped `generate-jitconfig` 200 response. Its
/// `encoded_jit_config` is a genuine base64 3-blob envelope.
const RESPONSE_FIXTURE: &str = include_str!("fixtures/generate_jitconfig_response.json");

/// The runner id encoded in the fixture's `runner.id`.
const FIXTURE_RUNNER_ID: i64 = 461;
/// The `client_id` embedded in the fixture's decoded `.credentials` blob.
const FIXTURE_CLIENT_ID: &str = "toolu-runner-fixture-client";

#[test]
fn build_request_matches_generate_jitconfig_contract() {
  let labels = vec![
    "self-hosted".to_owned(),
    "linux".to_owned(),
    "x64".to_owned(),
  ];
  let req = build_request(
    "https://github.com/octo-org/octo-repo",
    "runner-1",
    &labels,
    None,
    "_work",
  )
  .expect("build_request should succeed for a valid repo URL");

  assert_eq!(
    req.url, "https://api.github.com/repos/octo-org/octo-repo/actions/runners/generate-jitconfig",
    "github.com targets the api.github.com repo endpoint"
  );
  assert_eq!(req.body.name, "runner-1");
  assert_eq!(
    req.body.runner_group_id, 1,
    "default group id is 1 (Default)"
  );
  assert_eq!(req.body.work_folder, "_work");
  assert_eq!(req.body.labels, labels);

  // The serialized body must use GitHub's exact field names.
  let json: Value = serde_json::to_value(&req.body).expect("body serializes");
  assert_eq!(json.get("name"), Some(&Value::from("runner-1")));
  assert_eq!(json.get("runner_group_id"), Some(&Value::from(1)));
  assert_eq!(json.get("work_folder"), Some(&Value::from("_work")));
  assert_eq!(
    json.get("labels").and_then(|l| l.get(0)),
    Some(&Value::from("self-hosted"))
  );
}

#[test]
fn build_request_handles_ghes_dot_git_and_explicit_group() {
  let req = build_request(
    "https://ghe.example.com/acme/widgets.git",
    "r",
    &[],
    Some(7),
    "_work",
  )
  .expect("GHES build_request should succeed");

  assert_eq!(
    req.url, "https://ghe.example.com/api/v3/repos/acme/widgets/actions/runners/generate-jitconfig",
    "GHES targets <host>/api/v3 and strips a trailing .git"
  );
  assert_eq!(req.body.runner_group_id, 7);
}

#[test]
fn build_request_rejects_url_without_repo() {
  let err = build_request("https://github.com/just-owner", "r", &[], None, "_work")
    .expect_err("a URL missing the repo segment must error");
  assert!(
    matches!(err, RunnerError::Config(_)),
    "expected Config error, got {err:?}"
  );
}

#[test]
fn parse_response_extracts_runner_id_and_encoded_config() {
  let reg =
    parse_response(RESPONSE_FIXTURE, "runner-1").expect("the real-shaped fixture must parse");

  assert_eq!(reg.runner_id, FIXTURE_RUNNER_ID);
  assert_eq!(reg.runner_name, "runner-1");
  assert!(
    !reg.encoded_jit_config.is_empty(),
    "encoded_jit_config must be carried through"
  );

  // The encoded config is a genuine 3-blob envelope: it must decode and
  // expose the expected client_id (this is what `cmd_register` lifts).
  let jit = protocol::JitConfig::parse(&reg.encoded_jit_config)
    .expect("encoded_jit_config must be a parseable JIT envelope");
  assert_eq!(jit.credentials.data.client_id, FIXTURE_CLIENT_ID);
}

#[test]
fn parse_response_rejects_empty_encoded_config() {
  let body = r#"{"runner":{"id":5},"encoded_jit_config":""}"#;
  let err = parse_response(body, "r").expect_err("empty encoded_jit_config must error");
  assert!(
    matches!(err, RunnerError::Protocol(_)),
    "expected Protocol error, got {err:?}"
  );
}

#[test]
fn parse_response_rejects_malformed_json() {
  let err = parse_response("not json", "r").expect_err("malformed body must error");
  assert!(
    matches!(err, RunnerError::Protocol(_)),
    "expected Protocol error, got {err:?}"
  );
}

/// A non-dotcom (test-stub) repo URL whose api/v3 base IS the mock server.
/// `build_request` keeps the input scheme + host:port for non-github.com
/// hosts, so requests land on the local wiremock instance.
fn stub_repo_url(server: &MockServer) -> String {
  format!("{}/octo-org/octo-repo", server.uri())
}

/// The api/v3 path `build_request` derives for `octo-org/octo-repo`.
const STUB_PATH: &str = "/api/v3/repos/octo-org/octo-repo/actions/runners/generate-jitconfig";

#[tokio::test]
async fn register_jit_posts_bearer_body_and_parses_success() {
  let server = MockServer::start().await;
  let response_json: Value = serde_json::from_str(RESPONSE_FIXTURE).expect("fixture is valid JSON");

  let expected_body = serde_json::json!({
    "name": "runner-1",
    "runner_group_id": 1,
    "labels": ["self-hosted", "linux"],
    "work_folder": "_work",
  });

  Mock::given(method("POST"))
    .and(path(STUB_PATH))
    .and(header("authorization", "Bearer reg-token-xyz"))
    .and(header("accept", "application/vnd.github+json"))
    // GitHub's REST API requires a User-Agent or it 403s; this matcher
    // fails the call if the header regresses (regression guard for the
    // live-discovered 403 "User-Agent required").
    .and(header(
      "user-agent",
      concat!("toolu-runner/", env!("CARGO_PKG_VERSION")),
    ))
    .and(body_json(&expected_body))
    .respond_with(ResponseTemplate::new(201).set_body_json(response_json))
    .expect(1)
    .mount(&server)
    .await;

  let client = reqwest::Client::new();
  let url = stub_repo_url(&server);
  let labels = ["self-hosted".to_owned(), "linux".to_owned()];
  let reg = register_jit(
    &client,
    &RegisterParams {
      url: &url,
      runner_token: "reg-token-xyz",
      name: "runner-1",
      labels: &labels,
      runner_group_id: None,
      work_folder: "_work",
    },
  )
  .await
  .expect("register_jit should succeed and parse the stubbed response");

  assert_eq!(reg.runner_id, FIXTURE_RUNNER_ID);
  assert_eq!(reg.runner_name, "runner-1");
  let jit = protocol::JitConfig::parse(&reg.encoded_jit_config)
    .expect("the returned encoded_jit_config must parse");
  assert_eq!(jit.credentials.data.client_id, FIXTURE_CLIENT_ID);
}

#[tokio::test]
async fn register_jit_is_all_or_nothing_on_non_2xx() {
  let server = MockServer::start().await;

  Mock::given(method("POST"))
    .and(path(STUB_PATH))
    .respond_with(
      ResponseTemplate::new(403)
        .set_body_string(r#"{"message":"Must have admin rights to Repository."}"#),
    )
    .expect(1)
    .mount(&server)
    .await;

  let client = reqwest::Client::new();
  let url = stub_repo_url(&server);
  let labels = ["self-hosted".to_owned()];
  let err = register_jit(
    &client,
    &RegisterParams {
      url: &url,
      runner_token: "bad-token",
      name: "runner-1",
      labels: &labels,
      runner_group_id: None,
      work_folder: "_work",
    },
  )
  .await
  .expect_err("a 403 must yield an Err (all-or-nothing)");

  let msg = format!("{err}");
  assert!(
    matches!(err, RunnerError::Auth(_)),
    "non-2xx maps to Auth, got {err:?}"
  );
  assert!(msg.contains("403"), "status surfaced: {msg}");
  assert!(
    msg.contains("admin rights"),
    "GitHub's body surfaced: {msg}"
  );
}
