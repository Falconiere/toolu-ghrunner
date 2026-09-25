//! Acquired-message and real archive checks for issue #76 action downloads.

use base64::Engine;
use execution::execution::actions::download_info::ActionDownloadContext;
use execution::execution::actions::downloader::download_and_extract_action;
use execution::execution::actions::resolver::parse_action_ref;
use shared::{AgentJobRequestMessage, SecretMasker};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

#[test]
fn captured_run_service_job_targets_the_launch_resolution_api() {
  // Acquired GitHub.com job from defaults-run #71; its endpoint, plan, job,
  // and action reference are retained exactly, while credentials are sanitized.
  let msg: AgentJobRequestMessage = serde_json::from_str(include_str!("defaults_run_job.json"))
    .expect("captured acquired job parses");
  let context = ActionDownloadContext::from_message(&msg).expect("download context");
  let checkout = msg.steps.first().expect("captured checkout step");
  let action = parse_action_ref(&format!(
    "{}@{}",
    checkout.reference.name.as_deref().expect("action name"),
    checkout.reference.git_ref.as_deref().expect("action ref")
  ))
  .expect("checkout action ref");

  assert_eq!(
    context.launch_url().as_deref(),
    Some(
      "https://launch.actions.githubusercontent.com/actions/build/7207f5ec-6b47-4393-8102-fb0da0799922/jobs/bb41062b-c851-580e-8bb3-8fc916af5814/runnerresolve/actions"
    )
  );
  assert_eq!(
    context.launch_request(&[action]),
    serde_json::json!({"actions":[{"action":"actions/checkout","version":"v4"}]})
  );
  assert_eq!(context.api_url(), "https://api.github.com");
}

#[test]
fn captured_job_rejects_an_unsafe_launch_credential_target() {
  let mut raw: serde_json::Value =
    serde_json::from_str(include_str!("defaults_run_job.json")).expect("captured job JSON");
  let launch = raw
    .get_mut("variables")
    .and_then(|value| value.get_mut("system.github.launch_endpoint"))
    .and_then(|value| value.get_mut("value"))
    .expect("captured launch URL");
  *launch = serde_json::json!("http://ghe.example.internal");
  let msg: AgentJobRequestMessage = serde_json::from_value(raw).expect("boundary job");
  assert!(ActionDownloadContext::from_message(&msg).is_err());
}

#[test]
fn remote_ref_components_cannot_escape_archive_cache_or_rest_route() {
  for input in [
    "../checkout@v4",
    "actions/..@v4",
    "actions/checkout@../v4",
    "actions/checkout@feature/../x",
    "actions/checkout@v4?x=1",
  ] {
    assert!(parse_action_ref(input).is_err(), "accepted {input}");
  }
}

#[test]
fn capture_derived_enterprise_job_uses_its_own_api_host() {
  // Boundary transformation of the captured job: the original acquisition
  // proves the wire field locations; this variant checks host selection and
  // makes no claim that a GHES service response was captured.
  let mut raw: serde_json::Value =
    serde_json::from_str(include_str!("defaults_run_job.json")).expect("captured job JSON");
  raw
    .get_mut("variables")
    .and_then(serde_json::Value::as_object_mut)
    .expect("captured variables")
    .remove("system.github.launch_endpoint");
  let github = raw
    .get_mut("contextData")
    .and_then(|value| value.get_mut("github"))
    .and_then(|value| value.get_mut("d"))
    .and_then(serde_json::Value::as_array_mut)
    .expect("captured github context");
  for entry in github {
    if entry.get("k").and_then(serde_json::Value::as_str) == Some("api_url") {
      *entry.get_mut("v").expect("api_url value") =
        serde_json::json!("https://ghe.example.internal/api/v3");
    }
    if entry.get("k").and_then(serde_json::Value::as_str) == Some("server_url") {
      *entry.get_mut("v").expect("server_url value") =
        serde_json::json!("https://ghe.example.internal");
    }
  }
  let msg: AgentJobRequestMessage = serde_json::from_value(raw).expect("GHES boundary job");
  let context = ActionDownloadContext::from_message(&msg).expect("download context");

  assert_eq!(context.launch_url(), None);
  assert_eq!(context.api_url(), "https://ghe.example.internal/api/v3");
}

#[tokio::test]
async fn denied_launch_resolution_does_not_fall_back_to_an_archive_api() {
  // A controlled 403 is a failure-propagation probe, not a mock successful
  // service response. The request still originates from a captured job.
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
    .await
    .expect("bind response probe");
  let address = listener.local_addr().expect("probe address");
  let app = axum::Router::new().fallback(|| async { axum::http::StatusCode::FORBIDDEN });
  tokio::spawn(async move {
    let _ = axum::serve(listener, app).await;
  });

  let mut raw: serde_json::Value =
    serde_json::from_str(include_str!("defaults_run_job.json")).expect("captured job JSON");
  raw
    .get_mut("variables")
    .and_then(|value| value.get_mut("system.github.launch_endpoint"))
    .and_then(|value| value.get_mut("value"))
    .expect("captured launch endpoint")
    .clone_from(&serde_json::json!(format!("http://{address}")));
  let msg: AgentJobRequestMessage = serde_json::from_value(raw).expect("boundary job");
  let context = ActionDownloadContext::from_message(&msg).expect("download context");
  let action = parse_action_ref("actions/checkout@v4").expect("checkout ref");
  let masker = Arc::new(Mutex::new(SecretMasker::new()));
  let error = context
    .resolve(
      &reqwest::Client::new(),
      &action,
      &masker,
      &CancellationToken::new(),
    )
    .await
    .expect_err("403 must not fall back to a tarball API");
  assert!(error.to_string().contains("403"), "{error}");
}

#[tokio::test]
async fn absent_launch_endpoint_uses_the_acquired_api_host_and_job_token() {
  // Controlled 401 at the real REST revision-lookup route; no successful
  // service response is fabricated. The input envelope is capture-derived.
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
    .await
    .expect("bind REST probe");
  let address = listener.local_addr().expect("REST probe address");
  let (tx, mut rx) = tokio::sync::mpsc::channel::<(String, String)>(1);
  let app = axum::Router::new().fallback(move |request: axum::extract::Request| {
    let tx = tx.clone();
    async move {
      let path = request.uri().path().to_owned();
      let auth = request
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
      let _ = tx.send((path, auth)).await;
      axum::http::StatusCode::UNAUTHORIZED
    }
  });
  tokio::spawn(async move {
    let _ = axum::serve(listener, app).await;
  });

  let mut raw: serde_json::Value =
    serde_json::from_str(include_str!("defaults_run_job.json")).expect("captured job JSON");
  let vars = raw
    .get_mut("variables")
    .and_then(serde_json::Value::as_object_mut)
    .expect("captured variables");
  vars.remove("system.github.launch_endpoint");
  let expected_token = vars
    .get("system.github.token")
    .and_then(|value| value.get("value"))
    .and_then(serde_json::Value::as_str)
    .expect("captured job token")
    .to_owned();
  for entry in raw
    .get_mut("contextData")
    .and_then(|value| value.get_mut("github"))
    .and_then(|value| value.get_mut("d"))
    .and_then(serde_json::Value::as_array_mut)
    .expect("captured github context")
  {
    if entry.get("k").and_then(serde_json::Value::as_str) == Some("api_url") {
      *entry.get_mut("v").expect("api_url value") = serde_json::json!(format!("http://{address}"));
    }
  }
  let msg: AgentJobRequestMessage = serde_json::from_value(raw).expect("boundary job");
  let context = ActionDownloadContext::from_message(&msg).expect("download context");
  let action = parse_action_ref("actions/checkout@v4").expect("checkout ref");
  let masker = Arc::new(Mutex::new(SecretMasker::new()));
  let error = context
    .resolve(
      &reqwest::Client::new(),
      &action,
      &masker,
      &CancellationToken::new(),
    )
    .await
    .expect_err("REST auth denial must fail");
  assert!(error.to_string().contains("401"), "{error}");
  let (path, auth) = rx.recv().await.expect("REST request arrived");
  assert_eq!(path, "/repos/actions/checkout/commits/v4");
  assert_eq!(auth, format!("Bearer {expected_token}"));
}

#[tokio::test]
async fn archive_redirect_keeps_basic_auth_on_origin_and_drops_it_cross_origin() {
  let target_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
    .await
    .expect("bind target");
  let target_address = target_listener.local_addr().expect("target address");
  let (tx, mut rx) = tokio::sync::mpsc::channel::<Option<String>>(2);
  let target = axum::Router::new().fallback(move |request: axum::extract::Request| {
    let tx = tx.clone();
    async move {
      let auth = request
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
      let _ = tx.send(auth).await;
      axum::http::StatusCode::FORBIDDEN
    }
  });
  tokio::spawn(async move {
    let _ = axum::serve(target_listener, target).await;
  });

  let source_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
    .await
    .expect("bind source");
  let source_address = source_listener.local_addr().expect("source address");
  let (source_tx, mut source_rx) = tokio::sync::mpsc::channel::<Option<String>>(2);
  let source = axum::Router::new().fallback(move |request: axum::extract::Request| {
    let source_tx = source_tx.clone();
    async move {
      let auth = request
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
      let _ = source_tx.send(auth).await;
      (
        axum::http::StatusCode::FOUND,
        [(
          axum::http::header::LOCATION,
          format!("http://{target_address}/archive"),
        )],
      )
    }
  });
  tokio::spawn(async move {
    let _ = axum::serve(source_listener, source).await;
  });

  let token = "989329ac-43af-5d06-80e3-d5ce0deb6334";
  let destination =
    std::env::temp_dir().join(format!("toolu-action-redirect-{}", uuid::Uuid::new_v4()));
  let result = download_and_extract_action(
    &reqwest::Client::new(),
    &format!("http://{source_address}/tarball"),
    Some(token),
    &destination,
  )
  .await;
  let error = result.expect_err("terminal 403 must fail");
  assert!(error.to_string().contains("403"));
  assert!(!error.to_string().contains(token));
  let source_auth = source_rx.recv().await.expect("source request auth");
  let expected =
    base64::engine::general_purpose::STANDARD.encode(format!("x-access-token:{token}"));
  assert_eq!(
    source_auth.as_deref(),
    Some(format!("Basic {expected}").as_str())
  );
  assert_eq!(rx.recv().await.expect("target request auth"), None);
}
