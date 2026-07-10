//! Decrypt an encrypted broker message body end to end.
//!
//! Ties the session [`EncryptionKey`] together with the RSA-OAEP key unwrap
//! and the AES-256-CBC body codec. The poll path calls this when the session
//! negotiated encryption; plaintext bodies (the common github.com JIT case)
//! never reach here.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use shared::RunnerError;

use crate::messages::decrypt_message_body;
use crate::rsa_oaep::unwrap_aes_key_rsa_oaep;
use crate::session::EncryptionKey;

/// Recover the plaintext of an encrypted broker message body.
///
/// When `key.encrypted` the AES key is RSA-OAEP unwrapped with
/// `rsa_private_key_der`; otherwise `key.value` is the raw AES key. `fips`
/// selects the OAEP hash (SHA-256 when true, SHA-1 otherwise).
///
/// # Errors
///
/// Returns `RunnerError::Protocol` on base64, key-unwrap, or AES-CBC failure.
pub fn decrypt_broker_body(
  body_b64: &str,
  iv_b64: &str,
  key: &EncryptionKey,
  rsa_private_key_der: &[u8],
  fips: bool,
) -> Result<Vec<u8>, RunnerError> {
  let ciphertext = BASE64
    .decode(body_b64)
    .map_err(|e| RunnerError::Protocol(format!("broker body base64 decode failed: {e}")))?;
  let iv = BASE64
    .decode(iv_b64)
    .map_err(|e| RunnerError::Protocol(format!("broker IV base64 decode failed: {e}")))?;
  let key_bytes = BASE64
    .decode(&key.value)
    .map_err(|e| RunnerError::Protocol(format!("encryption key base64 decode failed: {e}")))?;

  let aes_key = if key.encrypted {
    unwrap_aes_key_rsa_oaep(&key_bytes, rsa_private_key_der, fips)?
  } else {
    key_bytes
  };

  decrypt_message_body(&aes_key, &iv, &ciphertext)
}
