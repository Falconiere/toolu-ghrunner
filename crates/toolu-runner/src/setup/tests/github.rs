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
    .respond_with(
      ResponseTemplate::new(200).set_body_json(serde_json::json!({"login":"acme","id":1})),
    )
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
  assert_eq!(
    err.code, 1,
    "403 must not render as a typo: {}",
    err.message
  );
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

#[tokio::test]
async fn repo_throttle_is_runtime_not_admin_gate() {
  let server = peer().await;
  Mock::given(method("GET"))
    .and(path("/user"))
    .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"login":"octo"})))
    .mount(&server)
    .await;
  Mock::given(method("GET"))
    .and(path("/repos/octo/web"))
    .respond_with(ResponseTemplate::new(429))
    .mount(&server)
    .await;
  let err = probe_token(
    &client(),
    &server.uri(),
    "tok",
    None,
    &["octo/web".to_owned()],
  )
  .await
  .unwrap_err();
  assert_eq!(
    err.code, 1,
    "429 on a repo check must be a retryable runtime error: {}",
    err.message
  );
}

#[tokio::test]
async fn membership_throttle_is_runtime_not_admin_gate() {
  let server = peer().await;
  Mock::given(method("GET"))
    .and(path("/user"))
    .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"login":"octo"})))
    .mount(&server)
    .await;
  Mock::given(method("GET"))
    .and(path("/orgs/limited/memberships/octo"))
    .respond_with(ResponseTemplate::new(403))
    .mount(&server)
    .await;
  let err = probe_token(&client(), &server.uri(), "tok", Some("limited"), &[])
    .await
    .unwrap_err();
  assert_eq!(
    err.code, 1,
    "403 must be a retryable runtime error: {}",
    err.message
  );
}

#[tokio::test]
async fn poll_online_waits_through_offline_then_online() {
  let server = peer().await;
  // Mounted last, wins until exhausted: two offline answers, then the
  // online mock underneath answers.
  Mock::given(method("GET"))
    .and(path("/api/v3/orgs/acme/actions/runners"))
    .respond_with(
      ResponseTemplate::new(200)
        .set_body_json(serde_json::json!({"total_count":1,"runners":[{"id":5,"name":"host-1","status":"online","busy":false,"labels":[]}]})),
    )
    .mount(&server)
    .await;
  Mock::given(method("GET"))
    .and(path("/api/v3/orgs/acme/actions/runners"))
    .respond_with(
      ResponseTemplate::new(200).set_body_json(serde_json::json!({"total_count":0,"runners":[]})),
    )
    .up_to_n_times(2)
    .mount(&server)
    .await;
  let url = format!("{}/acme", server.uri());
  let summary = poll_online(&client(), &url, "tok", "host-1", Duration::from_secs(30))
    .await
    .unwrap();
  assert_eq!(summary.id, 5);
}

#[tokio::test]
async fn poll_online_timeout_names_doctor() {
  let server = peer().await;
  Mock::given(method("GET"))
    .and(path("/api/v3/orgs/acme/actions/runners"))
    .respond_with(
      ResponseTemplate::new(200).set_body_json(serde_json::json!({"total_count":0,"runners":[]})),
    )
    .mount(&server)
    .await;
  let url = format!("{}/acme", server.uri());
  let err = poll_online(&client(), &url, "tok", "host-1", Duration::from_millis(1))
    .await
    .unwrap_err();
  assert_eq!(err.code, 1);
  assert!(err.message.contains("doctor"), "{}", err.message);
}

/// A saved App (or the same App imported from a file) resolves its
/// bearer from the peer's installation endpoints, in memory.
#[tokio::test]
async fn app_bearer_from_saved_and_imported_app() {
  use crate::setup::apply;
  use crate::setup::plan::{AuthPlan, PlanMode, PlanService, ResolvedPlan, Target};
  let server = peer().await;
  Mock::given(method("GET"))
    .and(path("/api/v3/orgs/acme/installation"))
    .respond_with(ResponseTemplate::new(200).set_body_json(
      serde_json::json!({"id":77,"account":{"login":"acme","id":1},"repository_selection":"all"}),
    ))
    .mount(&server)
    .await;
  Mock::given(method("POST"))
    .and(path("/api/v3/app/installations/77/access_tokens"))
    .respond_with(ResponseTemplate::new(201).set_body_json(
      serde_json::json!({"token":"ghs_peer_token","expires_at":"2099-01-01T00:00:00Z"}),
    ))
    .mount(&server)
    .await;
  let home = std::env::temp_dir().join(format!("toolu-setup-app-{}", std::process::id()));
  std::fs::create_dir_all(&home).unwrap();
  let app = config::app_store::StoredApp {
    host: "github.com".to_owned(),
    app_id: 4242,
    slug: "toolu-test".to_owned(),
    owner: "acme".to_owned(),
    client_id: "Iv1.test".to_owned(),
    client_secret: String::new(),
    webhook_secret: None,
    pem: include_str!("../../../../protocol/tests/fixtures/app_jwt_test_key.pem").to_owned(),
    html_url: "https://github.com/apps/toolu-test".to_owned(),
    created_at: "2026-01-01T00:00:00Z".to_owned(),
    owner_id: Some(1),
    installation_id: Some(77),
  };
  config::app_store::save_app(&home, &app).unwrap();
  let masker = std::sync::Mutex::new(shared::SecretMasker::new());
  let plan = |auth: AuthPlan| ResolvedPlan {
    github_base: server.uri(),
    host: "127.0.0.1".to_owned(),
    target: Target::Org {
      org: "acme".to_owned(),
    },
    auth,
    runner_group: None,
    name: "host-1".to_owned(),
    labels: Vec::new(),
    work_folder: String::new(),
    mode: PlanMode {
      ephemeral: false,
      replace: false,
      no_default_labels: false,
    },
    service: PlanService {
      install: false,
      enable_linger: false,
    },
  };
  let ctx = apply::scope_bearer(&plan(AuthPlan::SavedApp), &home, &client(), &masker, None)
    .await
    .unwrap();
  assert_eq!(ctx.bearer, "ghs_peer_token");
  assert!(ctx.app_installation);
  let imported = home.join("imported.json");
  std::fs::write(
    &imported,
    std::fs::read(config::app_store::app_path(&home)).unwrap(),
  )
  .unwrap();
  let ctx = apply::scope_bearer(
    &plan(AuthPlan::Import {
      file: Some(imported),
      app_id: None,
      key: None,
    }),
    &home,
    &client(),
    &masker,
    None,
  )
  .await
  .unwrap();
  assert_eq!(ctx.bearer, "ghs_peer_token");
  std::fs::remove_dir_all(&home).ok();
}
