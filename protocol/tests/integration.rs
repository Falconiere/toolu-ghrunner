//! Integration tests for the protocol crate.
//!
//! Synthetic but protocol-correct inputs. Real GH fixtures land in step 11.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use protocol::v1::{
  ConnectionData, LocationServiceData, ServiceDefinition, resolve_service_url, service_guids,
};
use protocol::{
  BrokerMessage, BrokerMigrationBody, JitConfig, MessageType, RsaKeyParams, RunnerJobRequestBody,
  RunnerSettings, decrypt_message_body, strip_bom, strip_pkcs7_padding,
};
use serde_json::json;

#[test]
fn jit_config_roundtrips_synthetic_blob() {
  // Build a synthetic JIT config matching GH's shape:
  // outer = base64({".runner": base64(json_runner_settings),
  //                 ".credentials": base64(json_credentials),
  //                 ".credentials_rsaparams": base64(json_rsa_params)})
  let runner_settings = RunnerSettings {
    agent_id: 42,
    agent_name: "test-runner".to_owned(),
    pool_id: 7,
    server_url: "https://pipelinesghubeus5.actions.githubusercontent.com/AAAAA/".to_owned(),
    server_url_v2: "https://vstst.action.github.com/AAAAA/".to_owned(),
    git_hub_url: "https://github.com/falconiere".to_owned(),
    work_folder: "_work".to_owned(),
  };
  let credentials_json = json!({
    "Scheme": "OAuth",
    "Data": {
      "ClientId": "abc123",
      "AuthorizationUrl": "https://github.com/login/oauth/access_token",
    }
  });
  let rsa_params_json = json!({
    "exponent": BASE64.encode([0x01, 0x00, 0x01]),
    "modulus":  BASE64.encode(vec![0u8; 256]),
    "d":        BASE64.encode(vec![0u8; 256]),
    "p":        BASE64.encode(vec![1u8; 128]),
    "q":        BASE64.encode(vec![1u8; 128]),
    "dp":       BASE64.encode(vec![0u8; 128]),
    "dq":       BASE64.encode(vec![0u8; 128]),
    "inverseQ": BASE64.encode(vec![0u8; 128]),
  });

  let outer = json!({
    ".runner":              BASE64.encode(serde_json::to_vec(&runner_settings).unwrap()),
    ".credentials":         BASE64.encode(credentials_json.to_string().as_bytes()),
    ".credentials_rsaparams": BASE64.encode(rsa_params_json.to_string().as_bytes()),
  });
  let encoded = BASE64.encode(outer.to_string().as_bytes());

  let parsed = JitConfig::parse(&encoded).expect("synthetic JIT config should parse");

  assert_eq!(parsed.runner_settings.agent_id, 42);
  assert_eq!(parsed.runner_settings.agent_name, "test-runner");
  assert_eq!(parsed.runner_settings.pool_id, 7);
  assert_eq!(parsed.credentials.scheme, "OAuth");
  assert_eq!(parsed.credentials.data.client_id, "abc123");
  assert_eq!(
    parsed.credentials.data.authorization_url,
    "https://github.com/login/oauth/access_token"
  );
  assert_eq!(
    parsed.rsa_key_params.exponent,
    BASE64.encode([0x01, 0x00, 0x01])
  );
}

#[test]
fn broker_message_roundtrips_camel_case() {
  // GH sends snake_case body fields but camelCase envelope fields.
  let raw = json!({
    "messageId": 99,
    "messageType": "RunnerJobRequest",
    "body": "encrypted-blob",
    "iv": "abcdef=="
  });
  let msg: BrokerMessage = serde_json::from_value(raw).expect("envelope deserializes");
  assert_eq!(msg.message_id, 99);
  assert_eq!(msg.message_type, MessageType::RunnerJobRequest);
  assert_eq!(msg.body, "encrypted-blob");
  assert_eq!(msg.iv.as_deref(), Some("abcdef=="));

  let body: RunnerJobRequestBody = serde_json::from_str(
    r#"{
      "runner_request_id": "11111111-2222-3333-4444-555555555555",
      "run_service_url": "https://example.com/run/123",
      "billing_owner_id": "owner-1"
    }"#,
  )
  .expect("body deserializes");
  assert_eq!(
    body.runner_request_id,
    "11111111-2222-3333-4444-555555555555"
  );
  assert_eq!(body.run_service_url, "https://example.com/run/123");
  assert_eq!(body.billing_owner_id, "owner-1");

  // Migration body — camelCase envelope field.
  let migration: BrokerMigrationBody = serde_json::from_value(json!({
    "brokerBaseUrl": "https://broker-2.example.com"
  }))
  .expect("migration body deserializes");
  assert_eq!(migration.broker_base_url, "https://broker-2.example.com");
}

#[test]
fn pkcs7_padding_strips_correctly_and_rejects_bad_inputs() {
  // Valid: 3-byte pad on 5-byte payload.
  let mut buf = vec![b'a', b'b', b'c', b'd', b'e', 3, 3, 3];
  strip_pkcs7_padding(&mut buf).expect("valid padding strips");
  assert_eq!(buf, b"abcde");

  // BOM prefix gets stripped after the padding goes away.
  let mut buf = vec![0xEF, 0xBB, 0xBF, b'x', b'y', 2, 2];
  strip_pkcs7_padding(&mut buf).expect("valid padding strips");
  assert_eq!(strip_bom(&buf), b"xy");

  // No BOM — strip_bom is a no-op.
  assert_eq!(strip_bom(b"plain"), b"plain");

  // Inconsistent pad bytes → Protocol error.
  // Last byte claims pad_len=3, but byte at offset 5 is 4, not 3.
  let mut buf = vec![b'a', b'b', b'c', b'd', 4, 3, 3];
  let err = strip_pkcs7_padding(&mut buf).expect_err("inconsistent pad rejected");
  assert!(err.to_string().contains("inconsistent PKCS7"));

  // pad_len == 0 → Protocol error.
  let mut buf = vec![b'a', 0];
  let err = strip_pkcs7_padding(&mut buf).expect_err("zero pad rejected");
  assert!(err.to_string().contains("invalid PKCS7"));

  // pad_len > data.len() → Protocol error.
  let mut buf = vec![b'a', 20];
  let err = strip_pkcs7_padding(&mut buf).expect_err("oversized pad rejected");
  assert!(err.to_string().contains("invalid PKCS7"));
}

#[test]
fn aes_cbc_roundtrip_with_pkcs7_stripping() {
  // We can't easily test decrypt_message_body against a real GH payload here,
  // but we can verify the helpers chain together: encrypt with AES-256-CBC +
  // PKCS7 padding, then decrypt + strip, and recover the plaintext.
  use aes::cipher::{BlockEncryptMut, KeyIvInit, block_padding::Pkcs7};

  let key = [0x42u8; 32];
  let iv = [0x24u8; 16];
  let plaintext = b"hello, broker";

  let encryptor = cbc::Encryptor::<aes::Aes256>::new_from_slices(&key, &iv).expect("key+iv ok");
  // Allocate a buffer large enough for plaintext + a full PKCS7 block.
  let mut buf = vec![0u8; plaintext.len() + 16];
  buf
    .get_mut(..plaintext.len())
    .expect("buffer sized to fit plaintext")
    .copy_from_slice(plaintext);
  let ciphertext_len = encryptor
    .encrypt_padded_mut::<Pkcs7>(&mut buf, plaintext.len())
    .expect("encrypt ok")
    .len();

  let decrypted = decrypt_message_body(
    &key,
    &iv,
    buf
      .get(..ciphertext_len)
      .expect("ciphertext slice in range"),
  )
  .expect("decrypt succeeds");
  assert_eq!(decrypted, plaintext);
}

#[test]
fn rsa_key_params_roundtrip_and_discovery_resolves_known_guids() {
  let rsa = RsaKeyParams {
    exponent: BASE64.encode([0x01, 0x00, 0x01]),
    modulus: BASE64.encode(vec![0u8; 256]),
    d: BASE64.encode(vec![0u8; 256]),
    p: BASE64.encode(vec![1u8; 128]),
    q: BASE64.encode(vec![1u8; 128]),
    dp: BASE64.encode(vec![0u8; 128]),
    dq: BASE64.encode(vec![0u8; 128]),
    inverse_q: BASE64.encode(vec![0u8; 128]),
  };
  let json = serde_json::to_string(&rsa).expect("serialize");
  // Verify the camelCase rename survives a roundtrip.
  assert!(json.contains("\"inverseQ\""));
  assert!(!json.contains("\"inverse_q\""));
  let parsed: RsaKeyParams = serde_json::from_str(&json).expect("deserialize");
  assert_eq!(parsed.exponent, rsa.exponent);
  assert_eq!(parsed.inverse_q, rsa.inverse_q);

  // Synthetic connection data with three of the V1 service GUIDs.
  let data = ConnectionData {
    instance_id: "instance-1".to_owned(),
    location_service_data: LocationServiceData {
      service_definitions: vec![
        ServiceDefinition {
          identifier: service_guids::TIMELINE.to_owned(),
          service_type: Some("Timeline".to_owned()),
          display_name: Some("Timeline Service".to_owned()),
          relative_path: Some("/_apis/distributedtask/hubs/Actions/Plans".to_owned()),
        },
        ServiceDefinition {
          identifier: service_guids::LOG_FILES.to_owned(),
          service_type: None,
          display_name: None,
          relative_path: Some("/_apis/distributedtask/hubs/Actions/Logs".to_owned()),
        },
        ServiceDefinition {
          // Unknown GUID — should not match anything.
          identifier: "00000000-0000-0000-0000-000000000000".to_owned(),
          service_type: None,
          display_name: None,
          relative_path: Some("/should/not/match".to_owned()),
        },
      ],
    },
  };

  let base = "https://ghes.example.com";
  let timeline =
    resolve_service_url(base, &data, service_guids::TIMELINE).expect("TIMELINE resolves");
  assert_eq!(
    timeline,
    "https://ghes.example.com/_apis/distributedtask/hubs/Actions/Plans"
  );

  let logs =
    resolve_service_url(base, &data, service_guids::LOG_FILES).expect("LOG_FILES resolves");
  assert_eq!(
    logs,
    "https://ghes.example.com/_apis/distributedtask/hubs/Actions/Logs"
  );

  // GUID lookup is case-insensitive.
  let upper = resolve_service_url(base, &data, &service_guids::TIMELINE.to_uppercase())
    .expect("uppercase GUID resolves");
  assert_eq!(upper, timeline);

  // Unknown GUID → None.
  let none = resolve_service_url(base, &data, "deadbeef-dead-beef-dead-beefdeadbeef");
  assert!(none.is_none());
}
