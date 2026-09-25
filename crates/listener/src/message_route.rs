//! Pure routing decision for a broker message type.
//!
//! Separated from the async poll loop so the "what does the runner do with
//! this message type" decision can be unit-tested without a live broker.

use protocol::messages::MessageType;

/// What the poll loop does with a received broker message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageRoute {
  /// `RunnerJobRequest` — acquire and run the job.
  AcquireJob,
  /// `BrokerMigration` — switch to the new broker URL and keep polling.
  Migrate,
  /// `JobCancellation` — cancel the in-flight token (no broker ack: the
  /// poll's `lastMessageId` cursor prevents redelivery).
  Cancel,
  /// `ForceTokenRefresh` — re-exchange the session OAuth token.
  RefreshToken,
  /// A known update/configuration message without a supported local updater.
  UnsupportedControl,
  /// `HostedRunnerShutdown` — end this listener lifecycle.
  Shutdown,
  /// An unrecognized future type — log and advance the broker cursor.
  SkipUnknown,
}

/// Map a broker message type to its routing decision.
pub fn route(message_type: &MessageType) -> MessageRoute {
  match message_type {
    MessageType::RunnerJobRequest => MessageRoute::AcquireJob,
    MessageType::BrokerMigration => MessageRoute::Migrate,
    MessageType::JobCancellation => MessageRoute::Cancel,
    MessageType::ForceTokenRefresh => MessageRoute::RefreshToken,
    MessageType::RunnerRefresh | MessageType::AgentRefresh | MessageType::RunnerRefreshConfig => {
      MessageRoute::UnsupportedControl
    },
    MessageType::HostedRunnerShutdown => MessageRoute::Shutdown,
    MessageType::Unknown(_) => MessageRoute::SkipUnknown,
  }
}
