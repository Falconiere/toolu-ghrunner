//! Live-read rules against a real loopback peer (wiremock): org 404 vs
//! 403, token probes, group fallback data, online polling.

use super::*;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn peer() -> MockServer {
  MockServer::start().await
}

fn client() -> reqwest::Client {
  reqwest::Client::new()
}

#[tokio::test]
async fn org_check_separates_missing_from_refused() {
  let server = peer().await;
  Mock::given(method("GET"))
    .and(path("/orgs/acme"))
    .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"login":"acme","id":1})))
    .mount(&server)
    .await;
  Mock::given(method("GET"))
    .and(path("/orgs/ghost"))
    .respond_with(ResponseTemplate::new(404))
    .mount(&server)
    .await;
  Mock::given(method("GET"))
    .and(path("/orgs/limited"))
    .respond_with(ResponseTemplate::new(403))
    .mount(&server)
    .await;
  let base = server.uri();
  assert_eq!(
    check_org(&client(), &base, "acme").await.unwrap(),
    OrgCheck::Found("acme".to_owned())
  );
  assert_eq!(
    check_org(&client(), &base, "ghost").await.unwrap(),
    OrgCheck::Missing
  );
  let err = check_org(&client(), &base, "limited").await.unwrap_err();
  assert_eq!(err.code, 1, "403 must not render as a typo: {}", err.message);
}

#[tokio::test]
async fn token_probe_gates_on_permissions_not_scopes() {
  let server = peer().await;
  Mock::given(method("GET"))
    .and(path("/user"))
    .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"login":"octo"})))
    .mount(&server)
    .await;
  Mock::given(method("GET"))
    .and(path("/repos/octo/web"))
    .respond_with(
      ResponseTemplate::new(200).set_body_json(serde_json::json!({"permissions":{"admin":false}})),
    )
    .mount(&server)
    .await;
  Mock::given(method("GET"))
    .and(path("/orgs/acme/memberships/octo"))
    .respond_with(
      ResponseTemplate::new(200)
        .set_body_json(serde_json::json!({"state":"active","role":"admin"})),
    )
    .mount(&server)
    .await;
  let base = server.uri();
  let err = probe_token(&client(), &base, "tok", None, &["octo/web".to_owned()])
    .await
    .unwrap_err();
  assert_eq!(err.code, 2);
  assert!(err.message.contains("not an admin"), "{}", err.message);
  let login = probe_token(&client(), &base, "tok", Some("acme"), &[])
    .await
    .unwrap();
  assert_eq!(login, "octo");
}

#[tokio::test]
async fn poll_online_retries_until_listed_online() {
  let server = peer().await;
  Mock::given(method("GET"))
    .and(path("/api/v3/orgs/acme/actions/runners"))
    .respond_with(
      ResponseTemplate::new(200).set_body_json(
        serde_json::json!({"total_count":1,"runners":[{"id":5,"name":"host-1","status":"online","busy":true,"labels":[{"id":1,"name":"self-hosted","type":"read-only"}]}]}),
      ),
    )
    .mount(&server)
    .await;
  let url = format!("{}/acme", server.uri());
  let summary = poll_online(&client(), &url, "tok", "host-1", Duration::from_secs(5))
    .await
    .unwrap();
  assert_eq!(summary.id, 5);
  assert_eq!(summary.labels, vec!["self-hosted".to_owned()]);
}
