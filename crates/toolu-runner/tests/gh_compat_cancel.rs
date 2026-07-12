//! S3 — JobCancellation message classification and routing.
//!
//! Real-data: deserializes broker messages from their wire JSON and asserts
//! the pure routing decision sends a cancellation to `Cancel` (the in-flight
//! token), while job-request and migration messages keep their behavior.

use protocol::BrokerMessage;
use protocol::messages::{JobCancelBody, MessageType};
use listener::message_route::{MessageRoute, route};

#[test]
fn job_cancellation_routes_to_cancel() {
  let msg: BrokerMessage = serde_json::from_str(
    r#"{"messageId":42,"messageType":"JobCancellation",
        "body":"{\"jobId\":\"11111111-2222-3333-4444-555555555555\",\"timeout\":\"00:05:00\"}",
        "iv":null}"#,
  )
  .expect("parse cancellation message");

  assert_eq!(msg.message_type, MessageType::JobCancellation);
  assert_eq!(route(&msg.message_type), MessageRoute::Cancel);

  // The body parses to the target job id the runner cancels.
  let cancel: JobCancelBody = serde_json::from_str(&msg.body).expect("parse cancel body");
  assert_eq!(cancel.job_id, "11111111-2222-3333-4444-555555555555");
}

#[test]
fn job_request_still_routes_to_acquire() {
  assert_eq!(
    route(&MessageType::RunnerJobRequest),
    MessageRoute::AcquireJob,
    "RunnerJobRequest must still acquire — cancel routing must not break it"
  );
}

#[test]
fn broker_migration_still_routes_to_migrate() {
  assert_eq!(
    route(&MessageType::BrokerMigration),
    MessageRoute::Migrate,
    "BrokerMigration must still migrate — cancel routing must not break it"
  );
}

#[test]
fn every_message_type_has_a_route() {
  // Exhaustiveness guard: each variant maps to a distinct, defined action.
  for (ty, want) in [
    (MessageType::RunnerJobRequest, MessageRoute::AcquireJob),
    (MessageType::BrokerMigration, MessageRoute::Migrate),
    (MessageType::JobCancellation, MessageRoute::Cancel),
  ] {
    assert_eq!(route(&ty), want);
  }
}
