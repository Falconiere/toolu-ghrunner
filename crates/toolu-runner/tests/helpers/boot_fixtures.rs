//! Shared fixtures for `boot_cmd_e2e_test.rs`: a real-keypair JIT config
//! envelope pointed at a local `wiremock` broker, plus the mocks for one
//! single-script-step job (the step's shell command is a parameter, so
//! callers can drive a success, a failure, or a long-running step through
//! the same broker lifecycle).
//!
//! Deliberately NOT shared with `listener_smoke_test.rs` (its equivalent
//! `real_jit_config_b64` / `mount_auth_and_session` / `mount_job_lifecycle`
//! helpers are private to that file, and it is untouched in-flight work) —
//! trimmed down to only what `boot`'s subprocess E2E test needs.
//!
//! Included via `#[path = "helpers/boot_fixtures.rs"] mod boot_fixtures;`,
//! matching `live_e2e.rs`'s `helpers/live_harness.rs` pattern (Cargo does
//! not compile `tests/helpers/**` as a test target of its own).

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use rsa::traits::{PrivateKeyParts, PublicKeyParts};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A JIT config carrying a real 2048-bit RSA keypair, so the listener
/// completes the full authenticate -> JWT -> token-exchange handshake
/// against a wiremock broker at `server_url_v2`.
pub fn real_jit_config_b64(server_url_v2: &str) -> Result<String, String> {
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
    "AgentId": 501,
    "AgentName": "toolu-runner-boot-e2e-test",
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

/// Mount the `OAuth2` token exchange + broker session creation — the two
/// requests every successful `GitHubListener::run` issues before polling.
pub async fn mount_auth_and_session(server: &MockServer) {
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

/// The fixture job message (`fixtures/job_message.json`, shared with
/// `listener_smoke_test.rs`), cut down to a single script step running
/// `script_command` so the job completes fast with a real `Conclusion`
/// determined by that command's exit status. Clears `resources` so
/// `connect_live_log` never attempts a live-log WebSocket in this sandbox.
fn boot_e2e_job_message(script_command: &str) -> Result<shared::AgentJobRequestMessage, String> {
  const JOB_MESSAGE: &str = include_str!("../fixtures/job_message.json");
  let mut msg: shared::AgentJobRequestMessage =
    serde_json::from_str(JOB_MESSAGE).map_err(|e| format!("parse job_message.json: {e}"))?;
  msg.steps = vec![shared::ActionStep::script("done", script_command, "")];
  msg.resources = shared::JobResources::default();
  Ok(msg)
}

/// Mount the full poll -> acquire -> acknowledge -> complete broker
/// exchange for one `RunnerJobRequest`, serving `boot_e2e_job_message`'s
/// single script step (running `script_command`) as the acquired job body.
pub async fn mount_job_lifecycle(server: &MockServer, script_command: &str) -> Result<(), String> {
  let job_body = json!({
    "runner_request_id": "22222222-3333-4444-5555-666677778888",
    "run_service_url": server.uri(),
    "billing_owner_id": "owner-boot-e2e-test",
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

  let job_msg = boot_e2e_job_message(script_command)?;
  Mock::given(method("POST"))
    .and(path("/acquirejob"))
    .respond_with(
      ResponseTemplate::new(200)
        .insert_header("x-plan-id", "plan-boot-e2e-test")
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
