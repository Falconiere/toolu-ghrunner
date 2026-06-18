use std::collections::HashMap;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use shared::RunnerError;

use super::types::{CredentialData, RsaKeyParams, RunnerSettings};

/// Decoded JIT config — the three base64-encoded blobs.
#[derive(Debug, Clone)]
pub struct JitConfig {
  pub runner_settings: RunnerSettings,
  pub credentials: CredentialData,
  pub rsa_key_params: RsaKeyParams,
}

impl JitConfig {
  /// Parse a base64-encoded JIT config string.
  ///
  /// The JIT config is a base64-encoded JSON object with three keys:
  /// - `.runner` — base64-encoded `RunnerSettings` JSON
  /// - `.credentials` — base64-encoded `CredentialData` JSON
  /// - `.credentials_rsaparams` — base64-encoded `RsaKeyParams` JSON
  ///
  /// # Errors
  ///
  /// Returns `RunnerError::Protocol` on decode or parse failures.
  pub fn parse(encoded: &str) -> Result<Self, RunnerError> {
    let outer_bytes = BASE64
      .decode(encoded)
      .map_err(|e| RunnerError::Protocol(format!("JIT config base64 decode failed: {e}")))?;

    let outer: HashMap<String, String> = serde_json::from_slice(&outer_bytes)
      .map_err(|e| RunnerError::Protocol(format!("JIT config JSON parse failed: {e}")))?;

    let runner_settings = decode_blob(&outer, ".runner", "runner settings")?;
    let credentials = decode_blob(&outer, ".credentials", "credentials")?;
    let rsa_key_params = decode_blob(&outer, ".credentials_rsaparams", "RSA key params")?;

    Ok(Self {
      runner_settings,
      credentials,
      rsa_key_params,
    })
  }
}

fn decode_blob<T: serde::de::DeserializeOwned>(
  outer: &HashMap<String, String>,
  key: &str,
  label: &str,
) -> Result<T, RunnerError> {
  let encoded = outer
    .get(key)
    .ok_or_else(|| RunnerError::Protocol(format!("JIT config missing key: {key}")))?;

  let bytes = BASE64
    .decode(encoded)
    .map_err(|e| RunnerError::Protocol(format!("{label} base64 decode failed: {e}")))?;

  serde_json::from_slice(&bytes).map_err(|e| RunnerError::Protocol(format!("{label} JSON parse failed: {e}")))
}
