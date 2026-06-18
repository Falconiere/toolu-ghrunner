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
  pub session_id: String,
  pub owner_name: String,
  pub agent: AgentInfo,
}

/// Agent information sent in session creation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentInfo {
  pub id: i64,
  pub name: String,
  pub version: String,
  pub os_description: String,
  pub ephemeral: bool,
}

/// Response from `POST /session`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateSessionResponse {
  pub session_id: String,
  pub owner_name: String,
  pub agent: Option<AgentInfo>,
  pub encryption_key: Option<EncryptionKey>,
}

/// Encryption key returned by session creation.
///
/// If `encrypted` is true, `value` is RSA-OAEP encrypted AES key (base64).
/// If false, `value` is the raw AES key (base64).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptionKey {
  pub encrypted: bool,
  pub value: String,
}

/// Lightweight session state held during the listener lifecycle.
#[derive(Debug, Clone)]
pub struct TaskAgentSession {
  pub session_id: String,
  pub encryption_key: Option<EncryptionKey>,
}

/// Build a `CreateSessionRequest` from runner settings.
pub fn build_session_request(agent_id: i64, agent_name: &str) -> CreateSessionRequest {
  let hostname = std::env::var("HOSTNAME")
    .or_else(|_| std::env::var("COMPUTERNAME"))
    .unwrap_or_else(|_| "unknown".to_owned());

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
