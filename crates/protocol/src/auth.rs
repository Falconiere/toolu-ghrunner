//! Sync crypto for the JIT auth flow: RSA key reconstruction, JWT signing.
//!
//! Network calls (`exchange_token`, `authenticate`) live in `toolu-runner::net`.
//! Keeping this module pure sync lets us unit-test the JWT + RSA math
//! without spinning up an HTTP client.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use num_bigint_dig::BigUint;
use pkcs1::der::Encode;
use pkcs1::{RsaPrivateKey, UintRef};
use serde::{Deserialize, Serialize};
use shared::RunnerError;

use super::types::RsaKeyParams;

/// OAuth2 token response from GitHub's token endpoint.
#[derive(Clone, Deserialize)]
pub struct AccessToken {
  pub access_token: String,
  pub expires_in: u64,
  pub token_type: String,
}

impl std::fmt::Debug for AccessToken {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("AccessToken")
      .field("access_token", &"<redacted>")
      .field("expires_in", &self.expires_in)
      .field("token_type", &self.token_type)
      .finish()
  }
}

/// JWT claims for GitHub Actions runner authentication.
#[derive(Debug, Serialize)]
struct JwtClaims {
  sub: String,
  iss: String,
  aud: String,
  jti: String,
  nbf: i64,
  iat: i64,
  exp: i64,
}

/// Parse an RSA private key from JIT config base64-encoded parameters
/// and return PKCS#1 DER-encoded bytes.
///
/// Parameters use .NET's `RSACryptoServiceProvider` format where each
/// component is a base64-encoded big-endian unsigned integer.
///
/// Computes the CRT parameters (dp, dq, qinv) from (d, p, q) to produce
/// a complete PKCS#1 RSAPrivateKey DER structure.
///
/// # Errors
///
/// Returns `RunnerError::Protocol` on decode or key construction failures.
pub fn parse_rsa_private_key(params: &RsaKeyParams) -> Result<Vec<u8>, RunnerError> {
  let n_bytes = decode_bytes(&params.modulus, "modulus")?;
  let e_bytes = decode_bytes(&params.exponent, "exponent")?;
  let d_bytes = decode_bytes(&params.d, "D")?;
  let p_bytes = decode_bytes(&params.p, "P")?;
  let q_bytes = decode_bytes(&params.q, "Q")?;

  // Compute CRT parameters: exponent1 = d mod (p-1), exponent2 = d mod (q-1),
  // coefficient = q^(-1) mod p (via Fermat's little theorem since p is prime).
  let (exp1_bytes, exp2_bytes, coeff_bytes) = compute_crt_params(&d_bytes, &p_bytes, &q_bytes);

  let n = uint_ref(&n_bytes, "modulus")?;
  let e = uint_ref(&e_bytes, "exponent")?;
  let d = uint_ref(&d_bytes, "D")?;
  let p = uint_ref(&p_bytes, "P")?;
  let q = uint_ref(&q_bytes, "Q")?;
  let exp1 = uint_ref(&exp1_bytes, "exponent1")?;
  let exp2 = uint_ref(&exp2_bytes, "exponent2")?;
  let coeff = uint_ref(&coeff_bytes, "coefficient")?;

  let private_key = RsaPrivateKey {
    modulus: n,
    public_exponent: e,
    private_exponent: d,
    prime1: p,
    prime2: q,
    exponent1: exp1,
    exponent2: exp2,
    coefficient: coeff,
    other_prime_infos: None,
  };

  private_key
    .to_der()
    .map_err(|err| RunnerError::Protocol(format!("PKCS#1 DER encoding failed: {err}")))
}

/// Build a JWT signed with PS256 for GitHub Actions OAuth2 token exchange.
///
/// Claims: `sub=clientId, iss=clientId, aud=authorizationUrl,
/// jti=uuid, nbf=now-30s, iat=now-30s, exp=now+4m30s`
/// GitHub measures lifetime as `exp - nbf`, capped at 5 minutes.
///
/// # Errors
///
/// Returns `RunnerError::Protocol` on encoding failures.
pub fn build_jwt(
  der_bytes: &[u8],
  client_id: &str,
  authorization_url: &str,
) -> Result<String, RunnerError> {
  let now = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map_err(|err| RunnerError::Protocol(format!("system time error: {err}")))?;
  let now_secs = i64::try_from(now.as_secs())
    .map_err(|err| RunnerError::Protocol(format!("timestamp overflow: {err}")))?;

  let claims = JwtClaims {
    sub: client_id.to_owned(),
    iss: client_id.to_owned(),
    aud: authorization_url.to_owned(),
    jti: uuid::Uuid::new_v4().to_string(),
    nbf: now_secs - 30,
    iat: now_secs - 30,
    exp: now_secs + 270,
  };

  let encoding_key = EncodingKey::from_rsa_der(der_bytes);
  let header = Header::new(Algorithm::PS256);

  jsonwebtoken::encode(&header, &claims, &encoding_key)
    .map_err(|err| RunnerError::Protocol(format!("JWT encoding failed: {err}")))
}

fn decode_bytes(encoded: &str, label: &str) -> Result<Vec<u8>, RunnerError> {
  BASE64
    .decode(encoded)
    .map_err(|err| RunnerError::Protocol(format!("{label} base64 decode failed: {err}")))
}

fn uint_ref<'a>(bytes: &'a [u8], label: &str) -> Result<UintRef<'a>, RunnerError> {
  UintRef::new(bytes)
    .map_err(|err| RunnerError::Protocol(format!("{label} ASN.1 integer encoding failed: {err}")))
}

/// Compute CRT (Chinese Remainder Theorem) parameters from RSA private key components.
///
/// Returns `(exponent1, exponent2, coefficient)` as big-endian byte vectors where:
/// - exponent1 = d mod (p - 1)
/// - exponent2 = d mod (q - 1)
/// - coefficient = q^(-1) mod p (via Fermat's little theorem since p is prime)
fn compute_crt_params(
  d_bytes: &[u8],
  p_bytes: &[u8],
  q_bytes: &[u8],
) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
  let d_val = BigUint::from_bytes_be(d_bytes);
  let p_val = BigUint::from_bytes_be(p_bytes);
  let q_val = BigUint::from_bytes_be(q_bytes);
  let one = BigUint::from(1u32);

  let exponent1 = &d_val % (&p_val - &one);
  let exponent2 = &d_val % (&q_val - &one);

  let p_minus_2 = &p_val - BigUint::from(2u32);
  let coefficient = q_val.modpow(&p_minus_2, &p_val);

  (
    exponent1.to_bytes_be(),
    exponent2.to_bytes_be(),
    coefficient.to_bytes_be(),
  )
}
