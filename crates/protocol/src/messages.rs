//! Broker message types + AES-256-CBC decryption.
//!
//! The async `poll_message` / `acknowledge_message` live in `toolu-runner::net`
//! because they talk HTTP. The crypto stays here so we can unit-test
//! padding stripping and BOM handling without a broker.

use serde::Deserialize;
use shared::RunnerError;

/// Message types from the broker long-poll.
///
/// Variant names are the wire `messageType` strings verbatim (serde uses
/// the variant identifier with no rename). `JobCancellation` matches the
/// C# runner's `JobCancelMessage.MessageType` discriminator.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub enum MessageType {
  /// A job to run, carried in a `RunnerJobRequestBody`.
  RunnerJobRequest,
  /// A broker migration notice, carried in a `BrokerMigrationBody`.
  BrokerMigration,
  /// A cancellation of an in-flight job, carried in a `JobCancelBody`.
  JobCancellation,
}

/// Raw message from the broker `GET /message` response.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrokerMessage {
  /// The `messageId` — must be echoed back to acknowledge the message.
  pub message_id: i64,
  /// The `messageType` — which body shape `body` decodes to.
  pub message_type: MessageType,
  /// The message payload: plaintext JSON, or ciphertext when `iv` is set.
  pub body: String,
  /// The AES initialization vector, present when `body` is encrypted.
  pub iv: Option<String>,
}

/// Body of a `RunnerJobRequest` message.
/// GitHub sends `snake_case` fields with `runner_request_id` as a string UUID.
#[derive(Debug, Clone, Deserialize)]
pub struct RunnerJobRequestBody {
  /// The `runner_request_id` — a string-encoded UUID identifying this job request.
  pub runner_request_id: String,
  /// The `run_service_url` — the Run Service base URL for acquiring/renewing/completing the job.
  pub run_service_url: String,
  /// The `billing_owner_id` — the id of the account billed for this job's usage.
  pub billing_owner_id: String,
}

/// Body of a `BrokerMigration` message.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrokerMigrationBody {
  /// The `brokerBaseUrl` — the new broker base URL to poll for future messages.
  pub broker_base_url: String,
}

/// Body of a `JobCancellation` message.
///
/// Mirrors the C# `JobCancelMessage`: the broker sends the `jobId` of the
/// run to cancel plus a grace `timeout` (a .NET `TimeSpan` string). The
/// listener cancels the in-flight token when this arrives.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobCancelBody {
  /// The `jobId` of the run to cancel.
  pub job_id: String,
  /// The grace `timeout` (a .NET `TimeSpan` string) before a hard cancel.
  pub timeout: Option<String>,
}

/// Decrypt a broker message body using AES-256-CBC.
///
/// # Errors
///
/// Returns `RunnerError::Protocol` on decryption or padding failures.
pub fn decrypt_message_body(
  aes_key: &[u8],
  iv: &[u8],
  ciphertext: &[u8],
) -> Result<Vec<u8>, RunnerError> {
  use aes::cipher::{BlockDecryptMut, KeyIvInit};

  let decryptor = cbc::Decryptor::<aes::Aes256>::new_from_slices(aes_key, iv)
    .map_err(|e| RunnerError::Protocol(format!("AES-CBC init failed: {e}")))?;

  let mut buf = ciphertext.to_vec();
  let decrypted = decryptor
    .decrypt_padded_mut::<aes::cipher::block_padding::NoPadding>(&mut buf)
    .map_err(|e| RunnerError::Protocol(format!("AES-CBC decrypt failed: {e}")))?;

  let mut result = decrypted.to_vec();
  strip_pkcs7_padding(&mut result)?;
  Ok(strip_bom(&result).to_vec())
}

/// Strip PKCS7 padding from decrypted data.
///
/// # Errors
///
/// Returns `RunnerError::Protocol` on invalid padding.
pub fn strip_pkcs7_padding(data: &mut Vec<u8>) -> Result<(), RunnerError> {
  let last = data
    .last()
    .copied()
    .ok_or_else(|| RunnerError::Protocol("empty data for PKCS7".to_owned()))?;

  let pad_len = usize::from(last);
  if pad_len == 0 || pad_len > 16 || pad_len > data.len() {
    return Err(RunnerError::Protocol(format!(
      "invalid PKCS7 padding: {pad_len}"
    )));
  }

  let start = data.len() - pad_len;
  for b in data.get(start..).unwrap_or_default() {
    if usize::from(*b) != pad_len {
      return Err(RunnerError::Protocol(format!(
        "inconsistent PKCS7: expected {pad_len}, got {b}"
      )));
    }
  }

  data.truncate(start);
  Ok(())
}

/// Strip UTF-8 BOM prefix if present.
pub fn strip_bom(data: &[u8]) -> &[u8] {
  let bom = [0xEF, 0xBB, 0xBF];
  if data.get(..3) == Some(&bom) {
    data.get(3..).unwrap_or_default()
  } else {
    data
  }
}
