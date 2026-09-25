//! Broker control routing with committed V2 envelope shapes.
//!
//! These tests exercise the production message classifier without a fake
//! broker service. A future type is a labelled capture-derived variant.

use protocol::BrokerMessage;
use protocol::messages::MessageType;

use crate::broker_message::{PollOutcome, classify_message};
use crate::job_lifecycle::is_redelivery;

const ENVELOPE: &str =
  include_str!("../../../toolu-runner/tests/fixtures/broker_message_job_request.json");

fn envelope_with_type(kind: &str) -> BrokerMessage {
  let mut raw: serde_json::Value =
    serde_json::from_str(ENVELOPE).expect("committed broker envelope parses");
  *raw
    .get_mut("messageType")
    .expect("message type field exists") = serde_json::Value::String(kind.to_owned());
  serde_json::from_value(raw).expect("capture-derived control envelope parses")
}

#[test]
fn future_type_is_skipped_with_the_original_cursor() {
  let message = envelope_with_type("FutureBrokerNotice");
  assert_eq!(
    message.message_type,
    MessageType::Unknown("FutureBrokerNotice".to_owned())
  );

  let outcome = classify_message(message);
  assert!(matches!(outcome, PollOutcome::Unknown { message_id: 99 }));
  assert_eq!(outcome.message_id(), Some(99));
}

#[test]
fn forced_refresh_is_distinct_from_future_types() {
  let outcome = classify_message(envelope_with_type("ForceTokenRefresh"));
  assert!(matches!(
    outcome,
    PollOutcome::RefreshToken { message_id: 99 }
  ));
  assert_eq!(outcome.message_id(), Some(99));
}

#[test]
fn known_update_messages_are_distinct_from_unknown() {
  for kind in ["RunnerRefresh", "AgentRefresh", "RunnerRefreshConfig"] {
    let outcome = classify_message(envelope_with_type(kind));
    assert!(
      matches!(
        outcome,
        PollOutcome::UnsupportedControl { message_id: 99, .. }
      ),
      "{kind} must use its known-control route"
    );
  }
}

#[test]
fn hosted_shutdown_has_an_explicit_route() {
  let outcome = classify_message(envelope_with_type("HostedRunnerShutdown"));
  assert!(matches!(outcome, PollOutcome::Shutdown { message_id: 99 }));
}

#[test]
fn redelivered_control_message_cannot_advance_or_execute_twice() {
  let outcome = classify_message(envelope_with_type("FutureBrokerNotice"));
  assert!(!is_redelivery(98, &outcome));
  assert!(is_redelivery(99, &outcome));
  assert!(is_redelivery(100, &outcome));
}

#[test]
fn future_type_then_job_reaches_acquire_route() {
  let unknown = classify_message(envelope_with_type("FutureBrokerNotice"));
  let cursor = unknown.message_id().expect("future message retains cursor");
  let mut raw: serde_json::Value =
    serde_json::from_str(ENVELOPE).expect("committed broker envelope parses");
  *raw.get_mut("messageId").expect("message ID field exists") = serde_json::json!(cursor + 1);
  let job: BrokerMessage = serde_json::from_value(raw).expect("job envelope parses");
  let outcome = classify_message(job);
  assert!(!is_redelivery(cursor, &outcome));
  assert!(matches!(outcome, PollOutcome::Job(_)));
}
