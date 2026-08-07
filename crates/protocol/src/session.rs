//! Session lifecycle types — request/response shapes and a pure builder.
//!
//! The async `create_session` / `delete_session` live in `toolu-runner::net`
//! because they hit the broker over HTTP. Keeping this module pure lets
//! `build_session_request` be unit-tested without an HTTP stack.

use serde::{Deserialize, Serialize};

/// Request body for `POST /session`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateSessionRequest {
  /// The `sessionId` to create — always the ephemeral all-zero UUID.
  pub session_id: String,
  /// The `ownerName` shown for this session (hostname + PID).
  pub owner_name: String,
  /// The `agent` info describing this runner.
  pub agent: AgentInfo,
}

/// Agent information sent in session creation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentInfo {
  /// The `id` — the runner's registered agent id.
  pub id: i64,
  /// The `name` — the runner's display name.
  pub name: String,
  /// The `version` — the reported runner version string.
  pub version: String,
  /// The `osDescription` — a human-readable OS/arch label.
  pub os_description: String,
  /// The `ephemeral` flag — always `true` for this runner.
  pub ephemeral: bool,
}

/// Response from `POST /session`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateSessionResponse {
  /// The `sessionId` the broker assigned to this session.
  pub session_id: String,
  /// The `ownerName` the broker recorded for this session.
  pub owner_name: String,
  /// The `agent` info the broker echoed back, if any.
  pub agent: Option<AgentInfo>,
  /// The `encryptionKey` for decrypting broker message bodies, if any.
  pub encryption_key: Option<EncryptionKey>,
  /// Whether the session uses FIPS-compliant encryption. When true the
  /// wrapped AES key uses RSA-OAEP-SHA256; otherwise OAEP-SHA1. Absent on
  /// github.com (defaults to non-FIPS).
  #[serde(default)]
  pub use_fips_encryption: bool,
}

/// Encryption key returned by session creation.
///
/// If `encrypted` is true, `value` is RSA-OAEP encrypted AES key (base64).
/// If false, `value` is the raw AES key (base64).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptionKey {
  /// Whether `value` is RSA-OAEP encrypted (`true`) or the raw AES key (`false`).
  pub encrypted: bool,
  /// The key material (base64), encrypted or raw per `encrypted`.
  pub value: String,
}

/// Lightweight session state held during the listener lifecycle.
#[derive(Debug, Clone)]
pub struct TaskAgentSession {
  /// The active session id used to poll and acknowledge messages.
  pub session_id: String,
  /// The session's encryption key, if the broker returned one.
  pub encryption_key: Option<EncryptionKey>,
}

/// Build a `CreateSessionRequest` from runner settings.
pub fn build_session_request(agent_id: i64, agent_name: &str) -> CreateSessionRequest {
  let hostname = crate::config::hostname().unwrap_or_else(|| "unknown".to_owned());

  CreateSessionRequest {
    session_id: "00000000-0000-0000-0000-000000000000".to_owned(),
    owner_name: format!("{hostname} (PID: {})", std::process::id()),
    agent: AgentInfo {
      id: agent_id,
      name: agent_name.to_owned(),
      version: "3.0.0".to_owned(),
      os_description: get_os_description(),
      ephemeral: true,
    },
  }
}

fn get_os_description() -> String {
  let os = std::env::consts::OS;
  let arch = std::env::consts::ARCH;
  format!("{os} {arch}")
}
