use serde::{Deserialize, Serialize};

/// Runner registration settings from `.runner` blob.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct RunnerSettings {
  /// The `AgentId` GitHub assigned to this runner registration.
  #[serde(deserialize_with = "string_or_i64")]
  pub agent_id: i64,
  /// The `AgentName` (the runner's display name on GitHub).
  pub agent_name: String,
  /// The `PoolId` this runner is registered into.
  #[serde(deserialize_with = "string_or_i64")]
  pub pool_id: i64,
  /// The `ServerUrl` — the Actions/pipelines service base URL.
  pub server_url: String,
  /// The `ServerUrlV2` — the V2 Actions/pipelines service base URL.
  #[serde(rename = "ServerUrlV2")]
  pub server_url_v2: String,
  /// The `GitHubUrl` — the repo or org URL this runner is registered against.
  pub git_hub_url: String,
  /// The `WorkFolder` — the relative work directory name for job checkouts.
  pub work_folder: String,
}

fn string_or_i64<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<i64, D::Error> {
  use serde::de;

  struct Visitor;

  impl de::Visitor<'_> for Visitor {
    type Value = i64;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
      f.write_str("an integer or string-encoded integer")
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<i64, E> {
      Ok(v)
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<i64, E> {
      i64::try_from(v).map_err(de::Error::custom)
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<i64, E> {
      v.parse().map_err(de::Error::custom)
    }
  }

  deserializer.deserialize_any(Visitor)
}

/// `OAuth2` credential data from `.credentials` blob.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct CredentialData {
  /// The `Scheme` — the credential scheme (e.g. `OAuth`).
  pub scheme: String,
  /// The `Data` — the `OAuth2` client id and authorization URL.
  pub data: CredentialDataInner,
}

/// Inner credential fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct CredentialDataInner {
  /// The `ClientId` used as the JWT `sub`/`iss` claim in the `OAuth2` token exchange.
  pub client_id: String,
  /// The `AuthorizationUrl` used as the JWT `aud` claim and the token endpoint.
  pub authorization_url: String,
}

/// RSA key parameters from `.credentials_rsaparams` blob.
/// All fields are base64-encoded big-endian integers.
/// GitHub sends these in camelCase (e.g. `exponent`, `modulus`, `inverseQ`).
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RsaKeyParams {
  /// The public exponent, base64-encoded big-endian.
  pub exponent: String,
  /// The RSA modulus (n), base64-encoded big-endian.
  pub modulus: String,
  /// The private exponent (d), base64-encoded big-endian.
  pub d: String,
  /// The first prime factor (p), base64-encoded big-endian.
  pub p: String,
  /// The second prime factor (q), base64-encoded big-endian.
  pub q: String,
  /// The first CRT exponent (`d mod (p-1)`), base64-encoded big-endian.
  pub dp: String,
  /// The second CRT exponent (`d mod (q-1)`), base64-encoded big-endian.
  pub dq: String,
  /// The CRT coefficient (`q^-1 mod p`), base64-encoded big-endian.
  pub inverse_q: String,
}

impl std::fmt::Debug for RsaKeyParams {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("RsaKeyParams")
      .field("modulus", &"<redacted>")
      .field("exponent", &"<redacted>")
      .field("d", &"<redacted>")
      .field("p", &"<redacted>")
      .field("q", &"<redacted>")
      .field("dp", &"<redacted>")
      .field("dq", &"<redacted>")
      .field("inverse_q", &"<redacted>")
      .finish()
  }
}
