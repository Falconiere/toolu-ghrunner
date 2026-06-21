//! S2 — message-body decryption contract on the poll path.
//!
//! The runner's poll loop calls `protocol::decrypt_broker_body` when the
//! session carries an `EncryptionKey`. This replays a real AES-256-CBC
//! ciphertext (with an RSA-OAEP wrapped key, and with a raw key) through the
//! exact public functions the poll path uses, and asserts the recovered
//! plaintext parses as a real broker body. No mocks.

use aes::cipher::{BlockEncryptMut, KeyIvInit, block_padding::Pkcs7};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use protocol::messages::RunnerJobRequestBody;
use protocol::session::EncryptionKey;
use protocol::{decrypt_broker_body, parse_rsa_private_key};
use rsa::pkcs1::DecodeRsaPrivateKey;
use rsa::{RsaPrivateKey, RsaPublicKey};
use sha1::Sha1;

/// AES-256-CBC encrypt with PKCS7 padding. Returns `Err` so callers keep
/// the `expect` inside their `#[test]` body (clippy `allow-expect-in-tests`).
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

const JOB_BODY: &[u8] = br#"{"runner_request_id":"abc-123","run_service_url":"https://run.example.com/x","billing_owner_id":"owner-9"}"#;

#[test]
fn encrypted_session_key_recovers_job_body() {
  let mut rng = rand::thread_rng();
  let private = RsaPrivateKey::new(&mut rng, 2048).expect("rsa key");
  let public = RsaPublicKey::from(&private);
  let der = rsa::pkcs1::EncodeRsaPrivateKey::to_pkcs1_der(&private)
    .expect("der")
    .as_bytes()
    .to_vec();

  let aes_key = [0x42u8; 32];
  let iv = [0x10u8; 16];
  let ciphertext = aes_encrypt(&aes_key, &iv, JOB_BODY).expect("aes encrypt");

  let wrapped = public
    .encrypt(&mut rng, rsa::oaep::Oaep::new::<Sha1>(), &aes_key)
    .expect("wrap aes key");

  let key = EncryptionKey {
    encrypted: true,
    value: BASE64.encode(&wrapped),
  };

  let plaintext = decrypt_broker_body(
    &BASE64.encode(&ciphertext),
    &BASE64.encode(iv),
    &key,
    &der,
    false, // non-FIPS → OAEP-SHA1, the github.com default
  )
  .expect("decrypt encrypted body");

  let body: RunnerJobRequestBody = serde_json::from_slice(&plaintext).expect("parse job body");
  assert_eq!(body.runner_request_id, "abc-123");
  assert_eq!(body.run_service_url, "https://run.example.com/x");
  assert_eq!(body.billing_owner_id, "owner-9");
}

#[test]
fn raw_session_key_recovers_job_body() {
  // encrypted=false → the session value is the raw AES key (no RSA needed).
  let aes_key = [0x99u8; 32];
  let iv = [0x20u8; 16];
  let ciphertext = aes_encrypt(&aes_key, &iv, JOB_BODY).expect("aes encrypt");

  let key = EncryptionKey {
    encrypted: false,
    value: BASE64.encode(aes_key),
  };

  let plaintext = decrypt_broker_body(
    &BASE64.encode(&ciphertext),
    &BASE64.encode(iv),
    &key,
    &[],
    false,
  )
  .expect("decrypt raw body");

  let body: RunnerJobRequestBody = serde_json::from_slice(&plaintext).expect("parse job body");
  assert_eq!(body.runner_request_id, "abc-123");
}

#[test]
fn runner_rsaparams_reconstruct_then_oaep_unwrap() {
  use protocol::RsaKeyParams;
  use protocol::unwrap_aes_key_rsa_oaep;
  use rsa::traits::PrivateKeyParts;
  use rsa::traits::PublicKeyParts;

  // Generate a real key and project it into the .NET-style RsaKeyParams the
  // JIT `credentials_rsaparams` blob carries (base64 big-endian integers).
  let mut rng = rand::thread_rng();
  let private = RsaPrivateKey::new(&mut rng, 2048).expect("rsa key");
  let public = RsaPublicKey::from(&private);

  let be = |n: &rsa::BigUint| BASE64.encode(n.to_bytes_be());
  let primes = private.primes();
  let p = primes.first().expect("prime p");
  let q = primes.get(1).expect("prime q");
  let params = RsaKeyParams {
    exponent: be(public.e()),
    modulus: be(public.n()),
    d: be(private.d()),
    p: be(p),
    q: be(q),
    // dp/dq/inverseQ are recomputed by parse_rsa_private_key; placeholders.
    dp: BASE64.encode([0u8]),
    dq: BASE64.encode([0u8]),
    inverse_q: BASE64.encode([0u8]),
  };

  // Runner path: rsaparams → PKCS#1 DER → OAEP unwrap.
  let der = parse_rsa_private_key(&params).expect("reconstruct rsa der");
  RsaPrivateKey::from_pkcs1_der(&der).expect("der decodes to rsa private key");

  let aes_key = [0xABu8; 32];
  let wrapped = public
    .encrypt(&mut rng, rsa::oaep::Oaep::new::<Sha1>(), &aes_key)
    .expect("wrap key");
  let unwrapped = unwrap_aes_key_rsa_oaep(&wrapped, &der, false).expect("unwrap with runner der");
  assert_eq!(
    unwrapped.as_slice(),
    aes_key.as_slice(),
    "runner-reconstructed key must unwrap the AES key"
  );
}
