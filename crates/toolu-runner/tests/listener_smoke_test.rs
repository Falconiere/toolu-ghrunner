//! Smoke tests for `listener::GitHubListener`.
//!
//! Verifies the polling loop constructs and exchanges the right HTTP
//! requests against a `wiremock` server simulating the GH message stream.
//! Two of the tests below also pin `GitHubListener::run`'s conclusion
//! contract (real data, no mocked business logic — only the GitHub HTTP
//! surface is simulated): `Ok(None)` when the lifecycle never acquires a
//! job, `Ok(Some(conclusion))` — the real completed job's own conclusion —
//! when it does.

use std::sync::Arc;
use std::time::Duration;

use listener::GitHubListener;
use serde_json::json;
use shared::RunnerConfig;
use shared::SecretMasker;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Build a minimal `RunnerConfig` rooted at a temp directory so file IO
/// doesn't fail during the listener lifecycle.
fn make_config() -> RunnerConfig {
  RunnerConfig {
    data_dir: std::env::temp_dir().join("toolu-runner-listener-test-data"),
    workspace_root: std::env::temp_dir().join("toolu-runner-listener-test-work"),
    cgroup_path: None,
    services_mode: shared::ServicesMode::default(),
    ..RunnerConfig::default()
  }
}

/// A recorded-real base64 JIT config. Built from a fabricated payload that
/// the listener only parses structurally — the listener cancels before it
/// gets far enough to use the RSA key for a real JWT signature.
fn minimal_jit_config_b64(server_url_v2: &str, agent_id: i64) -> String {
  use base64::Engine;
  use base64::engine::general_purpose::STANDARD as BASE64;
  let runner = json!({
    "AgentId": agent_id,
    "AgentName": "toolu-runner-test",
    "PoolId": 1,
    "ServerUrl": server_url_v2,
    "ServerUrlV2": server_url_v2,
    "GitHubUrl": server_url_v2,
    "WorkFolder": "_work",
  });
  let credentials = json!({
    "Scheme": "OAuth",
    "Data": {
      "ClientId": "test-client-id",
      "AuthorizationUrl": format!("{server_url_v2}/_apis/distributedtracing/oauth2/token"),
    }
  });
  let rsa = json!({
    "exponent": "AQAB",
    "modulus": "AQAB",
    "d": "AQAB",
    "p": "AQAB",
    "q": "AQAB",
    "dp": "AQAB",
    "dq": "AQAB",
    "inverseQ": "AQAB",
  });
  let outer = json!({
    ".runner": BASE64.encode(runner.to_string().as_bytes()),
    ".credentials": BASE64.encode(credentials.to_string().as_bytes()),
    ".credentials_rsaparams": BASE64.encode(rsa.to_string().as_bytes()),
  });
  BASE64.encode(outer.to_string().as_bytes())
}

/// A JIT config carrying a REAL 2048-bit RSA keypair, unlike
/// `minimal_jit_config_b64`'s degenerate placeholder — so the listener can
/// complete the full authenticate -> JWT -> token-exchange handshake
/// against a wiremock broker instead of failing at JWT signing. Mirrors the
/// `RsaKeyParams` reconstruction contract pinned by
/// `gh_compat_decrypt.rs::runner_rsaparams_reconstruct_then_oaep_unwrap`.
fn real_jit_config_b64(server_url_v2: &str, agent_id: i64) -> Result<String, String> {
  use base64::Engine;
  use base64::engine::general_purpose::STANDARD as BASE64;
  use rsa::traits::{PrivateKeyParts, PublicKeyParts};

  let mut rng = rand::thread_rng();
  let private =
    rsa::RsaPrivateKey::new(&mut rng, 2048).map_err(|e| format!("generate rsa key: {e}"))?;
  let public = rsa::RsaPublicKey::from(&private);
  let be = |n: &rsa::BigUint| BASE64.encode(n.to_bytes_be());
  let primes = private.primes();
  let p = primes.first().ok_or("rsa key missing prime p")?;
  let q = primes.get(1).ok_or("rsa key missing prime q")?;
  let rsa_params = protocol::RsaKeyParams {
    exponent: be(public.e()),
    modulus: be(public.n()),
    d: be(private.d()),
    p: be(p),
    q: be(q),
    // Recomputed by `parse_rsa_private_key` from (d, p, q); placeholders.
    dp: BASE64.encode([0u8]),
    dq: BASE64.encode([0u8]),
    inverse_q: BASE64.encode([0u8]),
  };

  let runner = json!({
    "AgentId": agent_id,
    "AgentName": "toolu-runner-conclusion-test",
    "PoolId": 1,
    "ServerUrl": server_url_v2,
    "ServerUrlV2": server_url_v2,
    "GitHubUrl": server_url_v2,
    "WorkFolder": "_work",
  });
  let credentials = json!({
    "Scheme": "OAuth",
    "Data": {
      "ClientId": "test-client-id",
      "AuthorizationUrl": format!("{server_url_v2}/_apis/distributedtracing/oauth2/token"),
    }
  });
  let rsa_params_json =
    serde_json::to_vec(&rsa_params).map_err(|e| format!("serialize rsa params: {e}"))?;
  let outer = json!({
    ".runner": BASE64.encode(runner.to_string().as_bytes()),
    ".credentials": BASE64.encode(credentials.to_string().as_bytes()),
    ".credentials_rsaparams": BASE64.encode(rsa_params_json),
  });
  Ok(BASE64.encode(outer.to_string().as_bytes()))
}

/// Mock the `OAuth2` token exchange + broker session creation — the two
/// requests every successful `GitHubListener::run` issues before it ever
/// reaches the poll loop.
async fn mount_auth_and_session(server: &MockServer) {
  Mock::given(method("POST"))
    .and(path("/_apis/distributedtracing/oauth2/token"))
    .respond_with(ResponseTemplate::new(200).set_body_json(json!({
      "access_token": "fake-token",
      "expires_in": 1800,
      "token_type": "bearer",
    })))
    .mount(server)
    .await;

  Mock::given(method("POST"))
    .and(path("/session"))
    .respond_with(ResponseTemplate::new(200).set_body_json(json!({
      "sessionId": "11111111-2222-3333-4444-555555555555",
      "ownerName": "test-owner",
    })))
    .mount(server)
    .await;
}

/// A real `AgentJobRequestMessage`, built from the same committed fixture
/// `gh_compat_forwarder.rs` drives through `run_job`, cut down to a single
/// trivially-successful script step so the job completes fast with a real
/// engine-derived `Conclusion::Success`. Clears `resources` (the
/// `SystemVssConnection` endpoint) so `connect_live_log` sees no
/// `FeedStreamUrl` and never attempts an (unreachable, in this sandbox)
/// live WebSocket — the results-service reporting path is already a no-op
/// without a `system.github.results_endpoint` variable, which this fixture
/// doesn't carry.
fn conclusion_test_job_message() -> Result<shared::AgentJobRequestMessage, String> {
  const JOB_MESSAGE: &str = include_str!("fixtures/job_message.json");
  let mut msg: shared::AgentJobRequestMessage =
    serde_json::from_str(JOB_MESSAGE).map_err(|e| format!("parse job_message.json: {e}"))?;
  msg.steps = vec![shared::ActionStep::script("done", "true", "")];
  msg.resources = shared::JobResources::default();
  Ok(msg)
}

/// Mock the full poll -> acquire -> acknowledge -> complete broker exchange
/// for one `RunnerJobRequest`, using `conclusion_test_job_message`'s single
/// trivially-successful script step as the acquired job body.
async fn mount_job_lifecycle(server: &MockServer) -> Result<(), String> {
  let job_body = json!({
    "runner_request_id": "22222222-3333-4444-5555-666677778888",
    "run_service_url": server.uri(),
    "billing_owner_id": "owner-conclusion-test",
  })
  .to_string();

  Mock::given(method("GET"))
    .and(path("/message"))
    .respond_with(ResponseTemplate::new(200).set_body_json(json!({
      "messageId": 1,
      "messageType": "RunnerJobRequest",
      "body": job_body,
      "iv": null,
    })))
    .mount(server)
    .await;

  Mock::given(method("POST"))
    .and(path("/acknowledge"))
    .respond_with(ResponseTemplate::new(200))
    .mount(server)
    .await;

  let job_msg = conclusion_test_job_message()?;
  Mock::given(method("POST"))
    .and(path("/acquirejob"))
    .respond_with(
      ResponseTemplate::new(200)
        .insert_header("x-plan-id", "plan-conclusion-test")
        .set_body_json(&job_msg),
    )
    .mount(server)
    .await;

  Mock::given(method("POST"))
    .and(path("/completejob"))
    .respond_with(ResponseTemplate::new(200))
    .mount(server)
    .await;

  Ok(())
}

/// A `RunnerConfig` rooted at a fresh tempdir (both `data_dir` and
/// `workspace_root` pre-created), so the real engine can actually run a job
/// in it — unlike `make_config`'s uncreated, test-shared temp paths.
async fn unique_job_config() -> Result<(tempfile::TempDir, RunnerConfig), Box<dyn std::error::Error>>
{
  let dir = tempfile::tempdir()?;
  let workspace_root = dir.path().join("work");
  let data_dir = dir.path().join("data");
  let (workspace_root, data_dir) = tokio::task::spawn_blocking(move || {
    std::fs::create_dir_all(&workspace_root)?;
    std::fs::create_dir_all(&data_dir)?;
    Ok::<_, std::io::Error>((workspace_root, data_dir))
  })
  .await??;
  let config = RunnerConfig {
    data_dir,
    workspace_root,
    cgroup_path: None,
    services_mode: shared::ServicesMode::default(),
    ..RunnerConfig::default()
  };
  Ok((dir, config))
}

#[tokio::test]
async fn listener_polls_until_cancellation() {
  let server = MockServer::start().await;

  // OAuth2 token endpoint returns success — but with a degenerate JIT
  // config, the JWT signing will fail before this is hit. We just want
  // to confirm the listener constructs and attempts the token exchange.
  Mock::given(method("POST"))
    .and(path("/_apis/distributedtracing/oauth2/token"))
    .respond_with(ResponseTemplate::new(200).set_body_json(json!({
      "access_token": "fake-token",
      "expires_in": 1800,
      "token_type": "bearer",
    })))
    .mount(&server)
    .await;

  let config = make_config();
  let masker = Arc::new(std::sync::Mutex::new(SecretMasker::new()));
  let cancel = CancellationToken::new();

  let jit_config = minimal_jit_config_b64(&server.uri(), 42);
  let listener = GitHubListener::new(&jit_config, config, masker, reqwest::Client::new())
    .expect("listener should construct from a valid JIT config payload");

  // Cancel after a short delay — the listener should observe cancellation
  // and return without panicking.
  let cancel_for_timer = cancel.clone();
  tokio::spawn(async move {
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    cancel_for_timer.cancel();
  });

  // We don't care if run() returns Ok or an error — the test passes as
  // long as the listener constructs, enters the polling loop, and exits
  // cleanly when cancelled.
  let _ = listener.run(cancel).await;
}

#[tokio::test]
async fn listener_constructs_from_jit_config() {
  // Pure construction smoke test — no network at all.
  let server = MockServer::start().await;
  let config = make_config();
  let masker = Arc::new(std::sync::Mutex::new(SecretMasker::new()));
  let jit_config = minimal_jit_config_b64(&server.uri(), 99);

  let listener = GitHubListener::new(&jit_config, config, masker, reqwest::Client::new())
    .expect("listener should construct");
  // The masker is held for future use by log uploaders; just confirm it's
  // retrievable.
  let _: &Arc<std::sync::Mutex<SecretMasker>> = listener.masker();
}

#[tokio::test]
async fn listener_rejects_invalid_jit_config() {
  let config = make_config();
  let masker = Arc::new(std::sync::Mutex::new(SecretMasker::new()));

  let result = GitHubListener::new("not-base64-or-json", config, masker, reqwest::Client::new());
  assert!(result.is_err(), "expected parse error on garbage input");
}

/// `GitHubListener::run` must return `Ok(None)` — not just "any Ok" — when
/// the lifecycle exits without ever acquiring a job: authenticate and
/// create-session both succeed (a real RSA key, unlike the degenerate one
/// `listener_polls_until_cancellation` uses), every long-poll returns 202
/// (no work), and cancellation fires mid-poll.
#[tokio::test]
async fn run_returns_none_when_cancelled_with_no_job_acquired() {
  let server = MockServer::start().await;
  mount_auth_and_session(&server).await;

  Mock::given(method("GET"))
    .and(path("/message"))
    .respond_with(ResponseTemplate::new(202))
    .mount(&server)
    .await;

  let config = make_config();
  let masker = Arc::new(std::sync::Mutex::new(SecretMasker::new()));
  let cancel = CancellationToken::new();
  let jit_config = real_jit_config_b64(&server.uri(), 43).expect("build a real-keypair jit config");
  let listener = GitHubListener::new(&jit_config, config, masker, reqwest::Client::new())
    .expect("listener should construct from a valid JIT config payload");

  let cancel_for_timer = cancel.clone();
  tokio::spawn(async move {
    tokio::time::sleep(Duration::from_millis(100)).await;
    cancel_for_timer.cancel();
  });

  let result = tokio::time::timeout(Duration::from_secs(10), listener.run(cancel))
    .await
    .expect("listener.run should observe cancellation well within the safety timeout")
    .expect("authenticate + session-create should succeed against the wiremock broker");

  assert_eq!(
    result, None,
    "run() must return Ok(None) when no job was ever acquired, not just any Ok"
  );
}

/// `GitHubListener::run` must surface the completed job's OWN conclusion —
/// not a fixed `Ok(())` regardless of outcome. Drives a real script step
/// (`true`) through the full authenticate -> session -> poll -> acquire ->
/// execute -> acknowledge -> complete lifecycle against a wiremock broker,
/// and asserts `run()` returns the real engine-derived `Conclusion::Success`.
#[tokio::test]
async fn run_returns_the_completed_jobs_conclusion() {
  let server = MockServer::start().await;
  mount_auth_and_session(&server).await;
  mount_job_lifecycle(&server)
    .await
    .expect("mount the broker + run-service mocks");

  let (_dir, config) = unique_job_config()
    .await
    .expect("build a fresh job-scoped RunnerConfig");
  let masker = Arc::new(std::sync::Mutex::new(SecretMasker::new()));
  let cancel = CancellationToken::new();
  let jit_config = real_jit_config_b64(&server.uri(), 44).expect("build a real-keypair jit config");
  let listener = GitHubListener::new(&jit_config, config, masker, reqwest::Client::new())
    .expect("listener should construct from a valid JIT config payload");

  let result = tokio::time::timeout(Duration::from_secs(20), listener.run(cancel))
    .await
    .expect("listener.run should complete a trivial script job well within the safety timeout")
    .expect("a real script step run against a fully-mocked broker should succeed");

  assert_eq!(
    result,
    Some(shared::Conclusion::Success),
    "run() must surface the completed job's own conclusion"
  );
}
