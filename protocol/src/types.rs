use serde::{Deserialize, Serialize};

/// Runner registration settings from `.runner` blob.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct RunnerSettings {
  #[serde(deserialize_with = "string_or_i64")]
  pub agent_id: i64,
  pub agent_name: String,
  #[serde(deserialize_with = "string_or_i64")]
  pub pool_id: i64,
  pub server_url: String,
  #[serde(rename = "ServerUrlV2")]
  pub server_url_v2: String,
  pub git_hub_url: String,
  pub work_folder: String,
}

fn string_or_i64<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<i64, D::Error> {
  use serde::de;

  struct Visitor;

  impl<'de> de::Visitor<'de> for Visitor {
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

/// OAuth2 credential data from `.credentials` blob.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct CredentialData {
  pub scheme: String,
  pub data: CredentialDataInner,
}

/// Inner credential fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct CredentialDataInner {
  pub client_id: String,
  pub authorization_url: String,
}

/// RSA key parameters from `.credentials_rsaparams` blob.
/// All fields are base64-encoded big-endian integers.
/// GitHub sends these in camelCase (e.g. `exponent`, `modulus`, `inverseQ`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RsaKeyParams {
  pub exponent: String,
  pub modulus: String,
  pub d: String,
  pub p: String,
  pub q: String,
  pub dp: String,
  pub dq: String,
  pub inverse_q: String,
}
