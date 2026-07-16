//! Regression tests for the CRT-parameter guard in `parse_rsa_private_key`.
//!
//! A crafted/spoofed `generate-jitconfig` response can carry degenerate RSA
//! primes (`p`/`q` <= 1) or a zero private exponent. Before the guard, the
//! `num-bigint-dig` CRT math (`p - 1`, `q - 1`, `p - 2`, `d % (p-1)`) panicked
//! with subtract-with-overflow / divide-by-zero, killing the runner (DoS).
//! These tests feed those values through the public API and assert we get an
//! `Err`, never a panic.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use protocol::{RsaKeyParams, parse_rsa_private_key};

/// Build `RsaKeyParams` from raw big-endian byte slices for `p`/`q`/`d`.
/// The unused CRT fields (`dp`/`dq`/`inverseQ`) are recomputed by the parser,
/// so any valid base64 will do for them.
fn params_with(p: &[u8], q: &[u8], d: &[u8]) -> RsaKeyParams {
  RsaKeyParams {
    exponent: BASE64.encode([0x01, 0x00, 0x01]),
    modulus: BASE64.encode(vec![0xFFu8; 256]),
    d: BASE64.encode(d),
    p: BASE64.encode(p),
    q: BASE64.encode(q),
    dp: BASE64.encode(vec![0u8; 128]),
    dq: BASE64.encode(vec![0u8; 128]),
    inverse_q: BASE64.encode(vec![0u8; 128]),
  }
}

/// A well-formed positive value for the operand under test (0x0202...02).
fn valid_large() -> Vec<u8> {
  vec![0x02u8; 128]
}

#[test]
fn rejects_prime_p_equal_to_one() {
  // p = 1 → `p - 1 == 0` → `d % (p-1)` would divide by zero.
  let params = params_with(&[0x01], &valid_large(), &valid_large());
  let err = parse_rsa_private_key(&params).expect_err("p == 1 must be rejected, not panic");
  assert!(
    err.to_string().contains("invalid RSA parameter P"),
    "unexpected error: {err}"
  );
}

#[test]
fn rejects_prime_p_equal_to_zero() {
  // p = 0 → `p - 1` and `p - 2` would underflow (BigUint has no negatives).
  // Empty bytes also decode to zero; use an explicit zero byte for clarity.
  let params = params_with(&[0x00], &valid_large(), &valid_large());
  let err = parse_rsa_private_key(&params).expect_err("p == 0 must be rejected, not panic");
  assert!(
    err.to_string().contains("invalid RSA parameter P"),
    "unexpected error: {err}"
  );
}

#[test]
fn rejects_prime_q_equal_to_one() {
  // q = 1 → `q - 1 == 0` → `d % (q-1)` would divide by zero.
  let params = params_with(&valid_large(), &[0x01], &valid_large());
  let err = parse_rsa_private_key(&params).expect_err("q == 1 must be rejected, not panic");
  assert!(
    err.to_string().contains("invalid RSA parameter Q"),
    "unexpected error: {err}"
  );
}

#[test]
fn rejects_zero_private_exponent() {
  // d = 0 is a nonsensical private exponent.
  let params = params_with(&valid_large(), &valid_large(), &[0x00]);
  let err = parse_rsa_private_key(&params).expect_err("d == 0 must be rejected, not panic");
  assert!(
    err.to_string().contains("invalid RSA parameter D"),
    "unexpected error: {err}"
  );
}

#[test]
fn valid_primes_pass_the_guard() {
  // Sanity check the guard does not reject well-formed positive operands:
  // large p, q, d clear every guard and reach the CRT math without panicking.
  let params = params_with(&valid_large(), &valid_large(), &valid_large());
  // The synthetic values are not a real key pair, so DER encoding may still
  // fail downstream — the contract here is only "no panic, no guard rejection".
  if let Err(err) = parse_rsa_private_key(&params) {
    let msg = err.to_string();
    assert!(
      !msg.contains("invalid RSA parameter"),
      "valid primes must clear the guard, got: {msg}"
    );
  }
}
