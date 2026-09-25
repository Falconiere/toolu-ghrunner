//! Broker envelope compatibility with future and known control types.
//!
//! The committed V2 broker envelope is used as the wire shape. A future
//! discriminator is a labelled capture-derived variant, not a GitHub capture.

use protocol::{BrokerMessage, MessageType};

const JOB_ENVELOPE: &str =
  include_str!("../../toolu-runner/tests/fixtures/broker_message_job_request.json");

#[test]
fn future_message_type_keeps_envelope_fields() {
  let mut raw: serde_json::Value =
    serde_json::from_str(JOB_ENVELOPE).expect("committed broker envelope parses");
  *raw
    .get_mut("messageType")
    .expect("message type field exists") =
    serde_json::Value::String("FutureBrokerNotice".to_owned());

  let message: BrokerMessage =
    serde_json::from_value(raw).expect("future type must not reject the envelope");

  assert_eq!(message.message_id, 99);
  assert_eq!(
    message.message_type,
    MessageType::Unknown("FutureBrokerNotice".to_owned())
  );
  assert!(!message.body.is_empty());
  assert_eq!(message.iv.as_deref(), Some("MTIzNDU2Nzg5MDEyMzQ1Ng=="));
}

#[test]
fn known_control_types_keep_envelope_fields() {
  for (kind, expected) in [
    ("ForceTokenRefresh", MessageType::ForceTokenRefresh),
    ("RunnerRefresh", MessageType::RunnerRefresh),
    ("AgentRefresh", MessageType::AgentRefresh),
    ("RunnerRefreshConfig", MessageType::RunnerRefreshConfig),
    ("HostedRunnerShutdown", MessageType::HostedRunnerShutdown),
  ] {
    let mut raw: serde_json::Value =
      serde_json::from_str(JOB_ENVELOPE).expect("committed broker envelope parses");
    *raw
      .get_mut("messageType")
      .expect("message type field exists") = serde_json::Value::String(kind.to_owned());
    let message: BrokerMessage = serde_json::from_value(raw).expect("known control type parses");
    assert_eq!(message.message_id, 99, "{kind}");
    assert_eq!(message.message_type, expected, "{kind}");
  }
}

#[test]
fn malformed_message_id_still_rejects_envelope() {
  let mut raw: serde_json::Value =
    serde_json::from_str(JOB_ENVELOPE).expect("committed broker envelope parses");
  *raw.get_mut("messageId").expect("message ID field exists") =
    serde_json::Value::String("not-an-integer".to_owned());
  assert!(serde_json::from_value::<BrokerMessage>(raw).is_err());
}
