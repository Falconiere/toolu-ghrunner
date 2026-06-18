use std::fmt;

use shared::AgentJobRequestMessage;

/// Protocol version detected from the job message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolVersion {
  /// GitHub.com protocol -- Twirp Results Service + signed blob log uploads.
  V2,
  /// GHES legacy protocol -- V1 timeline API with GUID-based routing.
  V1,
}

impl ProtocolVersion {
  pub fn is_v1(self) -> bool {
    matches!(self, Self::V1)
  }

  pub fn is_v2(self) -> bool {
    matches!(self, Self::V2)
  }
}

impl fmt::Display for ProtocolVersion {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::V1 => write!(f, "V1 (GHES Legacy)"),
      Self::V2 => write!(f, "V2 (GitHub.com)"),
    }
  }
}

/// Detect the protocol version from the job message.
///
/// If `run_service_url` is present and non-empty, use V2.
/// Otherwise fall back to V1 (GHES legacy timeline API).
pub fn detect_protocol_version(job: &AgentJobRequestMessage) -> ProtocolVersion {
  if job.run_service_url().is_some() {
    ProtocolVersion::V2
  } else {
    ProtocolVersion::V1
  }
}
