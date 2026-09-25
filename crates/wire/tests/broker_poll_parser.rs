//! The production poll parser accepts future broker discriminators.
//!
//! The input retains the committed V2 envelope shape; only the discriminator
//! changes to model a future broker control message.

use wire::net::messages::parse_broker_message;

const ENVELOPE: &str =
  include_str!("../../toolu-runner/tests/fixtures/broker_message_job_request.json");

#[test]
fn future_type_survives_the_poll_parser_with_cursor_fields() {
  let mut raw: serde_json::Value =
    serde_json::from_str(ENVELOPE).expect("committed broker envelope parses");
  *raw
    .get_mut("messageType")
    .expect("message type field exists") =
    serde_json::Value::String("FutureBrokerNotice".to_owned());
  let body = serde_json::to_vec(&raw).expect("encode capture-derived envelope");

  let message = parse_broker_message(&body).expect("poll parser accepts a future type");

  assert_eq!(message.message_id, 99);
  assert!(!message.body.is_empty());
  assert_eq!(message.iv.as_deref(), Some("MTIzNDU2Nzg5MDEyMzQ1Ng=="));
}

#[test]
fn malformed_id_still_fails_the_poll_parser() {
  let mut raw: serde_json::Value =
    serde_json::from_str(ENVELOPE).expect("committed broker envelope parses");
  *raw.get_mut("messageId").expect("message ID field exists") =
    serde_json::Value::String("not-an-integer".to_owned());
  let body = serde_json::to_vec(&raw).expect("encode capture-derived envelope");

  assert!(parse_broker_message(&body).is_err());
}
