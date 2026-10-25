//! #207 AC-10: the toolu.sh pairing, enrollment and self-service bodies.
//!
//! Inputs are the contract's own examples, verbatim, from CodaSignal/toolu.sh
//! `docs/guides/self-hosted-machines.md` ("Device-code pairing", "Enrollment
//! tokens", "Machine self-service").

use protocol::toolu_pair::{
  EnrollRequest, HostGrant, HostInfo, MachineReport, PairPoll, PairStart, PairTokenRequest,
  error_text, parse_pair_refusal,
};

/// `POST /runner/pair` 200 (guide example).
const PAIR_START: &str = r#"{
  "device_code": "Zm9vYmFyYmF6cXV4cXV1eGNvcmdlZ3JhdWx0Z2FycGx5",
  "user_code": "ABCD-2345",
  "verification_uri": "https://console.toolu.sh/pair",
  "verification_uri_complete": "https://console.toolu.sh/pair?code=ABCD-2345",
  "expires_in": 900,
  "interval": 5
}"#;

/// `POST /runner/pair/token` / `POST /runner/enroll` 200 (guide example).
const GRANT: &str = r#"{
  "host_token": "tlu_h_pairgrant207pairgrant207pairgrant207pairg",
  "host_id": "83dca27b-4052-420a-a767-04c5cd758654",
  "org": "org_123",
  "labels": ["gpu", "arm-box"]
}"#;

/// `GET /runner/host` 200 (guide example).
const HOST: &str = r#"{
  "host_id": "83dca27b-4052-420a-a767-04c5cd758654",
  "name": "Build box",
  "hostname": "worker-7",
  "org": { "id": "org_123", "name": "Example team" },
  "platform": "linux",
  "arch": "x64",
  "labels": ["GPU"],
  "slots": 2,
  "runner_version": "2.330.0",
  "enabled": true,
  "last_seen_at": 1792771200000
}"#;

fn report() -> MachineReport {
  MachineReport {
    hostname: "build-box".to_owned(),
    platform: "linux".to_owned(),
    arch: "x64".to_owned(),
    vcpu: Some(8),
    memory_mb: Some(32_768),
    labels: vec!["gpu".to_owned()],
    runner_version: "2.329.0".to_owned(),
  }
}

#[test]
fn the_machine_report_serializes_as_the_guide_shows() {
  let json = serde_json::to_value(report()).expect("serializes");
  let guide: serde_json::Value = serde_json::from_str(
    r#"{"hostname":"build-box","platform":"linux","arch":"x64","vcpu":8,"memory_mb":32768,"labels":["gpu"],"runner_version":"2.329.0"}"#,
  )
  .expect("guide body");
  assert_eq!(json, guide);
}

#[test]
fn unknown_capacity_is_omitted_not_guessed() {
  let mut machine = report();
  machine.vcpu = None;
  machine.memory_mb = None;
  let json = serde_json::to_value(machine).expect("serializes");
  assert!(
    json.get("vcpu").is_none() && json.get("memory_mb").is_none(),
    "{json}"
  );
}

#[test]
fn the_enroll_body_flattens_the_machine_report() {
  let body = EnrollRequest {
    enroll_token: "tlu_e_x".to_owned(),
    machine: report(),
  };
  let json = serde_json::to_value(&body).expect("serializes");
  for (key, want) in [
    ("enroll_token", "tlu_e_x"),
    ("hostname", "build-box"),
    ("runner_version", "2.329.0"),
  ] {
    assert_eq!(json.get(key).and_then(|v| v.as_str()), Some(want), "{key}");
  }
}

#[test]
fn the_pair_start_parses() {
  let start: PairStart = serde_json::from_str(PAIR_START).expect("parses");
  assert_eq!(start.user_code, "ABCD-2345");
  assert_eq!(start.verification_uri, "https://console.toolu.sh/pair");
  assert_eq!(
    start.verification_uri_complete,
    "https://console.toolu.sh/pair?code=ABCD-2345"
  );
  assert_eq!((start.expires_in, start.interval), (900, 5));
}

#[test]
fn the_grant_and_host_parse() {
  let grant: HostGrant = serde_json::from_str(GRANT).expect("grant parses");
  assert_eq!(grant.host_id, "83dca27b-4052-420a-a767-04c5cd758654");
  assert_eq!(grant.org, "org_123");
  assert_eq!(grant.labels, ["gpu", "arm-box"]);
  let host: HostInfo = serde_json::from_str(HOST).expect("host parses");
  assert_eq!(host.org.name, "Example team");
  assert!(host.enabled);
  assert_eq!(host.last_seen_at, Some(1_792_771_200_000));
}

#[test]
fn a_new_host_with_nulls_parses() {
  let body = HOST
    .replace(r#""worker-7""#, "null")
    .replace("1792771200000", "null")
    .replace("true", "false");
  let host: HostInfo = serde_json::from_str(&body).expect("parses");
  assert_eq!(
    (host.hostname, host.last_seen_at, host.enabled),
    (None, None, false)
  );
}

#[test]
fn every_pair_refusal_classifies() {
  let cases = [
    (r#"{"error":"authorization_pending"}"#, PairPoll::Pending),
    (r#"{"error":"slow_down"}"#, PairPoll::SlowDown),
    (r#"{"error":"access_denied"}"#, PairPoll::Denied),
    (r#"{"error":"expired_token"}"#, PairPoll::Expired),
    (
      r#"{"error":"rate_limited"}"#,
      PairPoll::Other("rate_limited".to_owned()),
    ),
    (
      "<html>bad gateway</html>",
      PairPoll::Other("<html>bad gateway</html>".to_owned()),
    ),
  ];
  for (body, want) in cases {
    assert_eq!(parse_pair_refusal(body), want, "{body}");
  }
}

#[test]
fn error_text_reads_the_field_or_keeps_the_body() {
  assert_eq!(
    error_text(r#"{"error":"invalid_enroll_token"}"#),
    "invalid_enroll_token"
  );
  assert_eq!(error_text("plain"), "plain");
}

#[test]
fn debug_never_prints_a_credential() {
  let start: PairStart = serde_json::from_str(PAIR_START).expect("parses");
  let grant: HostGrant = serde_json::from_str(GRANT).expect("parses");
  let enroll = EnrollRequest {
    enroll_token: "tlu_e_secretsecretsecret".to_owned(),
    machine: report(),
  };
  let poll = PairTokenRequest {
    device_code: start.device_code.clone(),
  };
  let printed = format!(
    "{start:?} {grant:?} {enroll:?} {poll:?} {:?}",
    PairPoll::Granted(grant.clone())
  );
  for secret in [&start.device_code, &grant.host_token, &enroll.enroll_token] {
    assert!(!printed.contains(secret.as_str()), "{printed}");
  }
  assert!(printed.contains("<redacted>"));
}

#[test]
fn the_patch_drops_platform_and_arch() {
  let json = serde_json::to_value(report().patch()).expect("serializes");
  let guide: serde_json::Value = serde_json::from_str(
    r#"{"hostname":"build-box","labels":["gpu"],"runner_version":"2.329.0","vcpu":8,"memory_mb":32768}"#,
  )
  .expect("guide body");
  assert_eq!(json, guide);
}
