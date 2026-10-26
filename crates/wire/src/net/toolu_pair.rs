//! toolu.sh pairing and enrollment calls (#207): how a box becomes a machine.
//!
//! These routes are public (no host token yet): `POST /runner/pair` starts a
//! device-code pairing, `POST /runner/pair/token` polls it, `POST
//! /runner/enroll` redeems a single-use enrollment token. Statuses map like
//! [`super::toolu_jobs`]: 401 [`TooluError::Revoked`] (a refused enrollment
//! token), 400 [`TooluError::Refused`] (refused input, e.g. a reserved
//! label), 429/5xx/transport [`TooluError::Transient`], anything else
//! [`TooluError::Protocol`]. Bodies and rules are `protocol::toolu_pair`; the
//! wait loop (interval, `slow_down`, expiry) is the caller's.

use protocol::toolu_pair::{
  EnrollRequest, HostGrant, MachineReport, PairPoll, PairStart, PairTokenRequest,
  parse_pair_refusal,
};
use reqwest::StatusCode;

use super::toolu_jobs::{TooluError, api_base, decode, exchange, failure, join};

/// `POST /runner/pair`: start pairing `machine`; the device and user codes.
///
/// # Errors
///
/// [`TooluError::Protocol`] for an unusable `api_url`, else [`TooluError`]
/// by status.
pub async fn pair_start(
  client: &reqwest::Client,
  api_url: &str,
  machine: &MachineReport,
) -> Result<PairStart, TooluError> {
  let url = endpoint(api_url, &["runner", "pair"])?;
  let (status, body) = exchange(client.post(url).json(machine), "start pairing").await?;
  if !status.is_success() {
    return Err(failure("start pairing", status, body));
  }
  decode(&body, "start pairing")
}

/// `POST /runner/pair/token`: one poll of `device_code`. A 400 carrying the
/// contract's `error` is an outcome, not an error.
///
/// # Errors
///
/// [`TooluError`] for any other non-2xx status or an unparseable 200.
pub async fn pair_token(
  client: &reqwest::Client,
  api_url: &str,
  device_code: &str,
) -> Result<PairPoll, TooluError> {
  let url = endpoint(api_url, &["runner", "pair", "token"])?;
  let request = client.post(url).json(&PairTokenRequest {
    device_code: device_code.to_owned(),
  });
  let (status, body) = exchange(request, "poll pairing").await?;
  match status {
    s if s.is_success() => decode(&body, "poll pairing").map(PairPoll::Granted),
    StatusCode::BAD_REQUEST => Ok(parse_pair_refusal(&body)),
    s => Err(failure("poll pairing", s, body)),
  }
}

/// `POST /runner/enroll`: redeem `enroll_token` for `machine`.
///
/// # Errors
///
/// [`TooluError::Revoked`] when toolu.sh refuses the token (unknown,
/// expired, revoked or already used: one answer for all), else by status.
pub async fn enroll(
  client: &reqwest::Client,
  api_url: &str,
  enroll_token: &str,
  machine: &MachineReport,
) -> Result<HostGrant, TooluError> {
  let url = endpoint(api_url, &["runner", "enroll"])?;
  let request = client.post(url).json(&EnrollRequest {
    enroll_token: enroll_token.to_owned(),
    machine: machine.clone(),
  });
  let (status, body) = exchange(request, "enroll").await?;
  if !status.is_success() {
    return Err(failure("enroll", status, body));
  }
  decode(&body, "enroll")
}

/// `<api_url>/<segments…>`; a bad `api_url` is a [`TooluError::Protocol`].
fn endpoint(api_url: &str, segments: &[&str]) -> Result<url::Url, TooluError> {
  let base = api_base(api_url).map_err(|e| TooluError::Protocol(e.to_string()))?;
  join(&base, segments)
}
