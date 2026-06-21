//! Real-data crypto tests for the broker message path.
//!
//! Covers S2 (RSA-OAEP AES-key unwrap + end-to-end body decrypt) and S3
//! (`JobCancellation` wire-shape classification). No mocks: a real RSA
//! keypair is generated and a real AES-256-CBC ciphertext is produced and
//! round-tripped through the production decrypt path.

use aes::cipher::{BlockEncryptMut, KeyIvInit, block_padding::Pkcs7};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use protocol::messages::JobCancelBody;
use protocol::session::EncryptionKey;
use protocol::{MessageType, decrypt_broker_body, decrypt_message_body, unwrap_aes_key_rsa_oaep};
use rsa::pkcs1::EncodeRsaPrivateKey;
use rsa::{RsaPrivateKey, RsaPublicKey};
use sha1::Sha1;
use sha2::Sha256;

/// A real 2048-bit RSA keypair plus its PKCS#1 DER, as the runner sees it.
struct TestKey {
  public: RsaPublicKey,
  der: Vec<u8>,
}

/// Generate a real 2048-bit keypair. Returns `Err` so callers keep the
/// `expect` inside their `#[test]` body (clippy `allow-expect-in-tests`).
fn gen_key() -> Result<TestKey, String> {
  let mut rng = rand::thread_rng();
  let private = RsaPrivateKey::new(&mut rng, 2048).map_err(|e| e.to_string())?;
  let public = RsaPublicKey::from(&private);
  let der = private
    .to_pkcs1_der()
    .map_err(|e| e.to_string())?
    .as_bytes()
    .to_vec();
  Ok(TestKey { public, der })
}

/// AES-256-CBC encrypt with PKCS7 padding, returning the ciphertext.
fn aes_encrypt(key: &[u8], iv: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, String> {
  let encryptor =
    cbc::Encryptor::<aes::Aes256>::new_from_slices(key, iv).map_err(|e| e.to_string())?;
  let mut buf = vec![0u8; plaintext.len() + 16];
  buf
    .get_mut(..plaintext.len())
    .ok_or("buffer too small")?
    .copy_from_slice(plaintext);
  Ok(
    encryptor
      .encrypt_padded_mut::<Pkcs7>(&mut buf, plaintext.len())
      .map_err(|e| e.to_string())?
      .to_vec(),
  )
}

#[test]
fn rsa_oaep_sha1_roundtrip_recovers_aes_key() {
  let key = gen_key().expect("gen key");
  let aes_key = [0x42u8; 32];

  let mut rng = rand::thread_rng();
  let wrapped = key
    .public
    .encrypt(&mut rng, rsa::oaep::Oaep::new::<Sha1>(), &aes_key)
    .expect("OAEP-SHA1 encrypt");

  let unwrapped = unwrap_aes_key_rsa_oaep(&wrapped, &key.der, false).expect("unwrap");
  assert_eq!(
    unwrapped, aes_key,
    "OAEP-SHA1 unwrap must recover the AES key"
  );
}

#[test]
fn rsa_oaep_sha256_roundtrip_recovers_aes_key_fips() {
  let key = gen_key().expect("gen key");
  let aes_key = [0x7fu8; 32];

  let mut rng = rand::thread_rng();
  let wrapped = key
    .public
    .encrypt(&mut rng, rsa::oaep::Oaep::new::<Sha256>(), &aes_key)
    .expect("OAEP-SHA256 encrypt");

  let unwrapped = unwrap_aes_key_rsa_oaep(&wrapped, &key.der, true).expect("unwrap fips");
  assert_eq!(
    unwrapped, aes_key,
    "OAEP-SHA256 (fips) unwrap must recover the AES key"
  );
}

#[test]
fn rsa_oaep_wrong_hash_fails() {
  let key = gen_key().expect("gen key");
  let aes_key = [0x11u8; 32];

  let mut rng = rand::thread_rng();
  // Wrap with SHA1 but try to unwrap as fips (SHA256) — must error, not panic.
  let wrapped = key
    .public
    .encrypt(&mut rng, rsa::oaep::Oaep::new::<Sha1>(), &aes_key)
    .expect("OAEP-SHA1 encrypt");

  let result = unwrap_aes_key_rsa_oaep(&wrapped, &key.der, true);
  assert!(result.is_err(), "hash mismatch must fail cleanly");
}

#[test]
fn decrypt_message_body_known_answer() {
  // Known AES-256 key + IV + plaintext → encrypt → decrypt back.
  let aes_key = [0x24u8; 32];
  let iv = [0x10u8; 16];
  let plaintext =
    br#"{"runner_request_id":"abc","run_service_url":"https://x","billing_owner_id":"o"}"#;

  let ciphertext = aes_encrypt(&aes_key, &iv, plaintext).expect("aes encrypt");
  let decrypted = decrypt_message_body(&aes_key, &iv, &ciphertext).expect("decrypt");
  assert_eq!(decrypted, plaintext, "AES-CBC KAT round-trip mismatch");
}

#[test]
fn decrypt_broker_body_encrypted_key_end_to_end() {
  // Full poll-path shape: encrypted EncryptionKey + base64 body/iv.
  let key = gen_key().expect("gen key");
  let aes_key = [0x55u8; 32];
  let iv = [0x33u8; 16];
  let plaintext = br#"{"brokerBaseUrl":"https://broker.example"}"#;

  let ciphertext = aes_encrypt(&aes_key, &iv, plaintext).expect("aes encrypt");

  let mut rng = rand::thread_rng();
  let wrapped = key
    .public
    .encrypt(&mut rng, rsa::oaep::Oaep::new::<Sha1>(), &aes_key)
    .expect("OAEP-SHA1 encrypt");

  let enc_key = EncryptionKey {
    encrypted: true,
    value: BASE64.encode(&wrapped),
  };

  let recovered = decrypt_broker_body(
    &BASE64.encode(&ciphertext),
    &BASE64.encode(iv),
    &enc_key,
    &key.der,
    false,
  )
  .expect("decrypt_broker_body");
  assert_eq!(
    recovered.as_slice(),
    plaintext.as_slice(),
    "end-to-end encrypted body mismatch"
  );
}

#[test]
fn decrypt_broker_body_raw_key_skips_rsa() {
  // encrypted=false → value is the raw AES key; RSA DER is unused.
  let aes_key = [0x66u8; 32];
  let iv = [0x77u8; 16];
  let plaintext = br#"{"brokerBaseUrl":"https://raw.example"}"#;

  let ciphertext = aes_encrypt(&aes_key, &iv, plaintext).expect("aes encrypt");
  let enc_key = EncryptionKey {
    encrypted: false,
    value: BASE64.encode(aes_key),
  };

  let recovered = decrypt_broker_body(
    &BASE64.encode(&ciphertext),
    &BASE64.encode(iv),
    &enc_key,
    &[], // no RSA key needed for a raw AES key
    false,
  )
  .expect("decrypt_broker_body raw");
  assert_eq!(
    recovered.as_slice(),
    plaintext.as_slice(),
    "raw-key path mismatch"
  );
}

#[test]
fn job_cancellation_message_type_deserializes() {
  // S3: the wire `messageType` string classifies to JobCancellation.
  let msg: protocol::BrokerMessage = serde_json::from_str(
    r#"{"messageId":7,"messageType":"JobCancellation","body":"{}","iv":null}"#,
  )
  .expect("parse broker message");
  assert_eq!(msg.message_type, MessageType::JobCancellation);
}

#[test]
fn job_cancel_body_parses_job_id_and_optional_timeout() {
  // Mirrors C# JobCancelMessage: { jobId, timeout }.
  let body: JobCancelBody = serde_json::from_str(
    r#"{"jobId":"deadbeef-dead-beef-dead-beefdeadbeef","timeout":"00:05:00"}"#,
  )
  .expect("parse cancel body");
  assert_eq!(body.job_id, "deadbeef-dead-beef-dead-beefdeadbeef");
  assert_eq!(body.timeout.as_deref(), Some("00:05:00"));

  // timeout is optional.
  let no_timeout: JobCancelBody =
    serde_json::from_str(r#"{"jobId":"abc"}"#).expect("parse cancel body without timeout");
  assert_eq!(no_timeout.job_id, "abc");
  assert!(no_timeout.timeout.is_none());
}
