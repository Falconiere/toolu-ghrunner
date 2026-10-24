//! #201 `wire::net::{github_app, runner_groups}` against a loopback peer
//! replaying GitHub's replies (`fixtures/github_app/`): the installation
//! lookup and token mint send a JWT that verifies with the test key's public
//! half, the documented replies parse, and every failure maps to the kind the
//! run loop acts on — `Auth` (fatal) for 401/403/404/422, `Network` (back
//! off) for 429/5xx and transport errors (AC-7/AC-8 at the wire layer).

use std::time::Duration;

use serde_json::json;
use shared::RunnerError;
use wire::net::RunnerScope;
use wire::net::github_app::{
  create_installation_token, find_installation, find_org, get_app, installation_repo_count,
  list_installations,
};
use wire::net::runner_groups::{list_runner_groups, pick_group};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::github_app_fixtures::{
  APP_ID, AppJwtGate, INSTALLATION_TOKEN, PRIVATE_PEM, authorizations, capture, edited_reply,
  rfc3339_in, token_reply, verify_app_jwt,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const TIMEOUT: Duration = Duration::from_secs(10);
const TOKENS_PATH: &str = "/api/v3/app/installations/1/access_tokens";

fn jwt() -> Result<String, RunnerError> {
  protocol::app_jwt::build_app_jwt(PRIVATE_PEM, APP_ID)
}

fn api(server: &MockServer) -> String {
  format!("{}/api/v3", server.uri())
}

fn repo() -> RunnerScope {
  RunnerScope::Repo {
    owner: "octo".to_owned(),
    repo: "demo".to_owned(),
  }
}

/// GitHub's documented basic 404 body (an App it knows, not installed here).
fn not_installed() -> ResponseTemplate {
  ResponseTemplate::new(404).set_body_json(json!({
    "message": "Not Found",
    "documentation_url": "https://docs.github.com/rest/apps/apps#get-a-repository-installation-for-the-authenticated-app",
    "status": "404",
  }))
}

#[tokio::test]
async fn the_repo_installation_resolves_with_a_verifiable_app_jwt() -> TestResult {
  let server = MockServer::start().await;
  let lookup = "/api/v3/repos/octo/demo/installation";
  Mock::given(method("GET"))
    .and(path(lookup))
    .respond_with(capture("installation_200.json")?)
    .expect(1)
    .mount(&server)
    .await;
  let client = reqwest::Client::new();
  let found = find_installation(&client, &api(&server), &jwt()?, &repo(), TIMEOUT).await?;
  assert_eq!(found.map(|installation| installation.id), Some(1));
  for authorization in authorizations(&server, lookup).await? {
    let claims = verify_app_jwt(&authorization)?;
    assert_eq!(
      claims.get("iss").and_then(serde_json::Value::as_str),
      Some("424242")
    );
  }
  Ok(())
}

#[tokio::test]
async fn an_org_scope_looks_up_the_org_installation() -> TestResult {
  let server = MockServer::start().await;
  Mock::given(method("GET"))
    .and(path("/api/v3/orgs/acme/installation"))
    .respond_with(capture("installation_200.json")?)
    .expect(1)
    .mount(&server)
    .await;
  let org = RunnerScope::Org {
    org: "acme".to_owned(),
  };
  let found = find_installation(
    &reqwest::Client::new(),
    &api(&server),
    &jwt()?,
    &org,
    TIMEOUT,
  )
  .await?;
  assert_eq!(found.map(|installation| installation.id), Some(1));
  Ok(())
}

#[tokio::test]
async fn a_known_app_not_installed_on_the_target_is_none() -> TestResult {
  let server = MockServer::start().await;
  Mock::given(method("GET"))
    .and(path("/api/v3/repos/octo/demo/installation"))
    .respond_with(not_installed())
    .mount(&server)
    .await;
  let found = find_installation(
    &reqwest::Client::new(),
    &api(&server),
    &jwt()?,
    &repo(),
    TIMEOUT,
  )
  .await?;
  assert_eq!(found, None);
  Ok(())
}

#[tokio::test]
async fn an_unknown_app_is_a_fatal_auth_error() -> TestResult {
  let server = MockServer::start().await;
  Mock::given(method("GET"))
    .respond_with(capture("app_404_integration_not_found.json")?)
    .mount(&server)
    .await;
  let err = find_installation(
    &reqwest::Client::new(),
    &api(&server),
    &jwt()?,
    &repo(),
    TIMEOUT,
  )
  .await
  .expect_err("an unknown App cannot be looked up");
  assert!(
    matches!(&err, RunnerError::Auth(msg) if msg.contains("does not know this App")),
    "{err:?}"
  );
  Ok(())
}

#[tokio::test]
async fn the_installation_list_parses_the_documented_reply() -> TestResult {
  let server = MockServer::start().await;
  Mock::given(method("GET"))
    .and(path("/api/v3/app/installations"))
    .and(query_param("per_page", "100"))
    .respond_with(capture("installations_200.json")?)
    .expect(1)
    .mount(&server)
    .await;
  let list = list_installations(&reqwest::Client::new(), &api(&server), &jwt()?, TIMEOUT).await?;
  let logins: Vec<String> = list
    .into_iter()
    .filter_map(|installation| installation.account.map(|account| account.login))
    .collect();
  assert_eq!(logins, ["octocat"]);
  Ok(())
}

#[tokio::test]
async fn an_installation_token_is_minted_with_a_verifiable_app_jwt() -> TestResult {
  let server = MockServer::start().await;
  let expires = rfc3339_in(3600);
  Mock::given(method("POST"))
    .and(path(TOKENS_PATH))
    // GitHub's REST API 403s a request without a User-Agent.
    .and(header("accept", "application/vnd.github+json"))
    .and(header("x-github-api-version", "2022-11-28"))
    .and(header(
      "user-agent",
      concat!("toolu-runner/", env!("CARGO_PKG_VERSION")),
    ))
    .respond_with(token_reply(&expires)?)
    .expect(1)
    .mount(&server)
    .await;
  let token =
    create_installation_token(&reqwest::Client::new(), &api(&server), &jwt()?, 1, TIMEOUT).await?;
  assert_eq!(token.token, INSTALLATION_TOKEN);
  assert_eq!(token.expires_at, expires);
  for authorization in authorizations(&server, TOKENS_PATH).await? {
    verify_app_jwt(&authorization)?;
  }
  Ok(())
}

/// Mint once against `reply` and return the error.
async fn mint_error(reply: ResponseTemplate) -> Result<RunnerError, Box<dyn std::error::Error>> {
  let server = MockServer::start().await;
  Mock::given(method("POST"))
    .and(path(TOKENS_PATH))
    .respond_with(reply)
    .mount(&server)
    .await;
  create_installation_token(&reqwest::Client::new(), &api(&server), &jwt()?, 1, TIMEOUT)
    .await
    .err()
    .ok_or_else(|| "the mint unexpectedly succeeded".into())
}

#[tokio::test]
async fn a_rejected_jwt_is_a_fatal_auth_error_quoting_github() -> TestResult {
  // Live capture: GitHub cannot verify a JWT signed with a revoked key.
  let err = mint_error(capture("access_tokens_401_bad_jwt.json")?).await?;
  assert!(
    matches!(&err, RunnerError::Auth(msg) if msg.contains("401") && msg.contains("A JSON web token could not be decoded")),
    "{err:?}"
  );
  Ok(())
}

#[tokio::test]
async fn a_deleted_app_or_installation_is_a_fatal_auth_error() -> TestResult {
  let err = mint_error(capture("app_404_integration_not_found.json")?).await?;
  assert!(
    matches!(&err, RunnerError::Auth(msg) if msg.contains("404")),
    "{err:?}"
  );
  for status in [403, 422] {
    let err = mint_error(ResponseTemplate::new(status).set_body_string("{}")).await?;
    assert!(matches!(err, RunnerError::Auth(_)), "{status}: {err:?}");
  }
  Ok(())
}

#[tokio::test]
async fn rate_limits_and_server_errors_are_transient() -> TestResult {
  for status in [429, 500, 503] {
    let err = mint_error(ResponseTemplate::new(status).set_body_string("unavailable")).await?;
    assert!(
      matches!(&err, RunnerError::Network(msg) if msg.contains("transient")),
      "{status}: {err:?}"
    );
  }
  Ok(())
}

#[tokio::test]
async fn a_dead_endpoint_is_a_transient_network_error() -> TestResult {
  let closed = std::net::TcpListener::bind("127.0.0.1:0")?;
  let base = format!("http://{}/api/v3", closed.local_addr()?);
  drop(closed);
  let err = create_installation_token(&reqwest::Client::new(), &base, &jwt()?, 1, TIMEOUT)
    .await
    .expect_err("nothing listens");
  assert!(matches!(err, RunnerError::Network(_)), "{err:?}");
  Ok(())
}

#[tokio::test]
async fn an_empty_token_is_a_protocol_error() -> TestResult {
  let err = mint_error(
    ResponseTemplate::new(201).set_body_json(json!({"token": "", "expires_at": rfc3339_in(3600)})),
  )
  .await?;
  assert!(matches!(err, RunnerError::Protocol(_)), "{err:?}");
  Ok(())
}

#[tokio::test]
async fn runner_groups_list_and_resolve_a_name() -> TestResult {
  let server = MockServer::start().await;
  Mock::given(method("GET"))
    .and(path("/api/v3/orgs/octo-org/actions/runner-groups"))
    .and(query_param("per_page", "100"))
    .respond_with(capture("runner_groups_200.json")?)
    .expect(1)
    .mount(&server)
    .await;
  let groups = list_runner_groups(
    &reqwest::Client::new(),
    &api(&server),
    "octo-org",
    INSTALLATION_TOKEN,
    TIMEOUT,
  )
  .await?;
  assert_eq!(groups.len(), 3);
  assert_eq!(pick_group(&groups, "expensive-hardware"), Some(3));
  let bearer = authorizations(&server, "/api/v3/orgs/octo-org/actions/runner-groups").await?;
  assert_eq!(bearer, [format!("Bearer {INSTALLATION_TOKEN}")]);
  Ok(())
}

#[tokio::test]
async fn a_bearer_that_cannot_read_runner_groups_is_an_auth_error() -> TestResult {
  let server = MockServer::start().await;
  Mock::given(method("GET"))
    .respond_with(ResponseTemplate::new(403).set_body_json(json!({
      "message": "Resource not accessible by integration",
      "status": "403",
    })))
    .mount(&server)
    .await;
  let err = list_runner_groups(
    &reqwest::Client::new(),
    &api(&server),
    "octo-org",
    "tok",
    TIMEOUT,
  )
  .await
  .expect_err("403");
  assert!(
    matches!(&err, RunnerError::Auth(msg) if msg.contains("Resource not accessible")),
    "{err:?}"
  );
  Ok(())
}

/// #202: `GET /app` with the App's own key returns its identity (documented
/// example, id rewritten to the test App).
#[tokio::test]
async fn get_app_returns_the_app_the_jwt_names() -> TestResult {
  let server = MockServer::start().await;
  let reply = edited_reply("app_200.json", |body| body["id"] = json!(APP_ID))?;
  Mock::given(method("GET"))
    .and(path("/api/v3/app"))
    .respond_with(AppJwtGate(reply))
    .mount(&server)
    .await;
  let app = get_app(&reqwest::Client::new(), &api(&server), &jwt()?, TIMEOUT).await?;
  assert_eq!(app.id, APP_ID);
  assert_eq!(app.slug, "octoapp");
  Ok(())
}

/// #202 S3 at the wire layer: a key the App does not hold gets GitHub's live
/// 401, a fatal `Auth` error quoting it.
#[tokio::test]
async fn get_app_with_a_key_the_app_does_not_hold_is_a_fatal_auth_error() -> TestResult {
  let server = MockServer::start().await;
  Mock::given(method("GET"))
    .and(path("/api/v3/app"))
    .respond_with(AppJwtGate(ResponseTemplate::new(200)))
    .mount(&server)
    .await;
  // Signed by the test key but naming another App: GitHub cannot verify it.
  let other = protocol::app_jwt::build_app_jwt(PRIVATE_PEM, 999)?;
  let err = get_app(&reqwest::Client::new(), &api(&server), &other, TIMEOUT)
    .await
    .err()
    .ok_or("a mismatched key must be refused")?;
  let RunnerError::Auth(msg) = err else {
    return Err(format!("expected Auth, got {err:?}").into());
  };
  assert!(msg.contains("401"), "{msg}");
  assert!(msg.contains("A JSON web token could not be decoded"), "{msg}");
  Ok(())
}

/// #202: the org check sends no credential and reads the live 200 capture;
/// the live 404 capture (a typo) is `Ok(None)`.
#[tokio::test]
async fn find_org_reads_the_org_and_treats_404_as_absent() -> TestResult {
  let server = MockServer::start().await;
  Mock::given(method("GET"))
    .and(path("/api/v3/orgs/CodaSignal"))
    .respond_with(capture("orgs_200.json")?)
    .mount(&server)
    .await;
  Mock::given(method("GET"))
    .and(path("/api/v3/orgs/toolu-no-such-org-202"))
    .respond_with(capture("orgs_404.json")?)
    .mount(&server)
    .await;
  let client = reqwest::Client::new();
  let org = find_org(&client, &api(&server), "CodaSignal", TIMEOUT)
    .await?
    .ok_or("the org exists")?;
  assert_eq!((org.login.as_str(), org.id), ("CodaSignal", 212_445_385));
  let missing = find_org(&client, &api(&server), "toolu-no-such-org-202", TIMEOUT).await?;
  assert_eq!(missing, None);
  let requests = server.received_requests().await.ok_or("recording off")?;
  assert!(
    requests
      .iter()
      .all(|request| request.headers.get("authorization").is_none()),
    "the org check must be unauthenticated"
  );
  Ok(())
}

/// #202: a rate-limited or failing org check is transient (`Network`).
#[tokio::test]
async fn find_org_server_errors_are_transient() -> TestResult {
  let server = MockServer::start().await;
  Mock::given(method("GET"))
    .and(path("/api/v3/orgs/acme"))
    .respond_with(ResponseTemplate::new(503))
    .mount(&server)
    .await;
  let err = find_org(&reqwest::Client::new(), &api(&server), "acme", TIMEOUT)
    .await
    .err()
    .ok_or("503 must fail")?;
  assert!(matches!(err, RunnerError::Network(_)), "{err:?}");
  Ok(())
}

/// #202: the selected-repository count comes from `total_count`, read with
/// the installation token.
#[tokio::test]
async fn installation_repo_count_reads_total_count_with_the_token() -> TestResult {
  let server = MockServer::start().await;
  Mock::given(method("GET"))
    .and(path("/api/v3/installation/repositories"))
    .and(query_param("per_page", "1"))
    .and(header(
      "authorization",
      format!("Bearer {INSTALLATION_TOKEN}").as_str(),
    ))
    .respond_with(capture("installation_repositories_200.json")?)
    .expect(1)
    .mount(&server)
    .await;
  let count = installation_repo_count(
    &reqwest::Client::new(),
    &api(&server),
    INSTALLATION_TOKEN,
    TIMEOUT,
  )
  .await?;
  assert_eq!(count, 1);
  Ok(())
}
