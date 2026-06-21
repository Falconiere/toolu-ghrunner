//! RSA-OAEP key unwrap for the session encryption key.
//!
//! When the broker session carries an `encrypted` AES key, that key is
//! wrapped with the runner's RSA public key using OAEP padding. The runner
//! reconstructs its RSA private key from the JIT `credentials_rsaparams`
//! blob (`auth::parse_rsa_private_key` → PKCS#1 DER) and unwraps the AES
//! key here. The recovered AES key then drives `decrypt_message_body`.
//!
//! Kept in its own module so `messages.rs` stays focused on the AES-CBC
//! body codec and the broker message shapes.

use rsa::RsaPrivateKey;
use rsa::oaep::Oaep;
use rsa::pkcs1::DecodeRsaPrivateKey;
use sha1::Sha1;
use sha2::Sha256;
use shared::RunnerError;

/// Unwrap an RSA-OAEP wrapped AES key with the runner's private key.
///
/// `private_key_der` is the PKCS#1 DER produced by
/// [`crate::auth::parse_rsa_private_key`]. `fips` selects the OAEP hash:
/// FIPS sessions use SHA-256, non-FIPS sessions use SHA-1 — matching the
/// C# runner's `UseFipsEncryption ? OaepSHA256 : OaepSHA1`.
///
/// # Errors
///
/// Returns `RunnerError::Protocol` if the DER cannot be decoded or the
/// OAEP decryption fails (wrong key, corrupt ciphertext, bad padding).
pub fn unwrap_aes_key_rsa_oaep(
  wrapped: &[u8],
  private_key_der: &[u8],
  fips: bool,
) -> Result<Vec<u8>, RunnerError> {
  let private_key = RsaPrivateKey::from_pkcs1_der(private_key_der)
    .map_err(|e| RunnerError::Protocol(format!("RSA private key DER decode failed: {e}")))?;

  let padding = if fips {
    Oaep::new::<Sha256>()
  } else {
    Oaep::new::<Sha1>()
  };

  private_key
    .decrypt(padding, wrapped)
    .map_err(|e| RunnerError::Protocol(format!("RSA-OAEP key unwrap failed: {e}")))
}
