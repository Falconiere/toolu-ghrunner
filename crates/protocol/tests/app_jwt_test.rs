//! S1 / AC-1 (#201): the GitHub App JWT against a real RSA key.
//!
//! `fixtures/app_jwt_test_key.pem` is a 2048-bit PKCS#1 key generated with
//! `openssl genrsa -traditional 2048` for these tests only. It was never
//! uploaded to any GitHub App, so it authenticates nothing; that is why it is
//! listed in this crate's `secrets.scanExempt`. The matching public key
//! verifies every signature here, so a JWT that only *looks* right fails.
//!
//! The reply-shape tests parse GitHub's documented examples (sanitized
//! token) from `toolu-runner/tests/fixtures/github_app/`.

use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use protocol::app_jwt::{
  APP_JWT_BACKDATE_SECS, APP_JWT_LIFETIME_SECS, AppInfo, Installation, InstallationToken, OrgInfo,
  build_app_jwt, build_app_jwt_at,
};
use serde_json::Value;

const PRIVATE_PEM: &str = include_str!("fixtures/app_jwt_test_key.pem");
const PUBLIC_PEM: &str = include_str!("fixtures/app_jwt_test_key.pub.pem");
const ACCESS_TOKENS_201: &str =
  include_str!("../../toolu-runner/tests/fixtures/github_app/access_tokens_201.json");
const INSTALLATION_200: &str =
  include_str!("../../toolu-runner/tests/fixtures/github_app/installation_200.json");
const INSTALLATIONS_200: &str =
  include_str!("../../toolu-runner/tests/fixtures/github_app/installations_200.json");
const APP_200: &str = include_str!("../../toolu-runner/tests/fixtures/github_app/app_200.json");
const ORGS_200: &str = include_str!("../../toolu-runner/tests/fixtures/github_app/orgs_200.json");

/// A fixed instant, so every claim is an exact value.
const NOW: i64 = 1_792_786_000;
const APP_ID: i64 = 123_456;

/// Verify `jwt` with the fixture's public key (signature + `RS256`), without
/// the expiry check (`NOW` is a fixed past instant), returning the claims.
fn verified_claims(jwt: &str) -> Result<Value, jsonwebtoken::errors::Error> {
  let key = DecodingKey::from_rsa_pem(PUBLIC_PEM.as_bytes())?;
  let mut validation = Validation::new(Algorithm::RS256);
  validation.validate_exp = false;
  validation.required_spec_claims.clear();
  Ok(decode::<Value>(jwt, &key, &validation)?.claims)
}

/// The `response.body` string of a capture fixture.
fn fixture_body(capture: &str) -> Result<String, Box<dyn std::error::Error>> {
  let value: Value = serde_json::from_str(capture)?;
  Ok(
    value
      .pointer("/response/body")
      .and_then(Value::as_str)
      .ok_or("fixture has no response.body")?
      .to_owned(),
  )
}

#[test]
fn jwt_header_and_claims_match_githubs_rules_and_the_signature_verifies() {
  let jwt = build_app_jwt_at(PRIVATE_PEM, APP_ID, NOW).expect("a real PKCS#1 key signs");

  let header = decode_header(&jwt).expect("header decodes");
  assert_eq!(header.alg, Algorithm::RS256);
  assert_eq!(header.typ.as_deref(), Some("JWT"));

  let claims = verified_claims(&jwt).expect("the fixture public key verifies the signature");
  let object = claims.as_object().expect("claims are an object");
  let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
  keys.sort_unstable();
  assert_eq!(keys, ["exp", "iat", "iss"], "exactly GitHub's three claims");
  assert_eq!(
    claims.get("iat").and_then(Value::as_i64),
    Some(NOW - 60),
    "iat is backdated 60 s for clock drift"
  );
  assert_eq!(
    claims.get("exp").and_then(Value::as_i64),
    Some(NOW + 540),
    "exp stays under GitHub's 10 minute cap"
  );
  assert_eq!(
    claims.get("iss").and_then(Value::as_str),
    Some("123456"),
    "iss is the App id"
  );
  assert_eq!(APP_JWT_BACKDATE_SECS, 60);
  // exp - iat must not exceed GitHub's 10 minutes.
  const { assert!(APP_JWT_BACKDATE_SECS + APP_JWT_LIFETIME_SECS <= 600) };
}

#[test]
fn a_tampered_payload_fails_verification() {
  let jwt = build_app_jwt_at(PRIVATE_PEM, APP_ID, NOW).expect("signs");
  let (header, rest) = jwt.split_once('.').expect("header");
  let (_payload, signature) = rest.split_once('.').expect("signature");
  let other = build_app_jwt_at(PRIVATE_PEM, 654_321, NOW).expect("signs");
  let other_payload = other.split('.').nth(1).expect("payload");
  // Another JWT's payload under this JWT's signature.
  let forged = format!("{header}.{other_payload}.{signature}");
  assert!(
    verified_claims(&forged).is_err(),
    "the signature must cover the claims"
  );
}

#[test]
fn build_app_jwt_signs_for_the_current_time() {
  let before = unix_now().expect("clock");
  let jwt = build_app_jwt(PRIVATE_PEM, APP_ID).expect("signs");
  let after = unix_now().expect("clock");
  let claims = verified_claims(&jwt).expect("verifies");
  let iat = claims
    .get("iat")
    .and_then(Value::as_i64)
    .expect("iat is a number");
  assert!(
    (before - 60..=after - 60).contains(&iat),
    "iat {iat} is now - 60 (now in {before}..={after})"
  );
}

#[test]
fn a_key_that_is_not_a_pem_is_an_error_that_does_not_echo_it() {
  let not_a_key = "MIIEnotapembutsecretlookingmaterial";
  let err = build_app_jwt_at(not_a_key, APP_ID, NOW).expect_err("not a PEM");
  assert!(
    !err.to_string().contains(not_a_key),
    "the error must not echo key material: {err}"
  );
}

/// A real key cut short (its PEM frame kept, half its body gone) is a
/// `Config` error, as the run loop's fatal path expects, and echoes no line
/// of the key.
#[test]
fn a_truncated_real_key_is_a_config_error_that_does_not_echo_it() {
  let lines: Vec<&str> = PRIVATE_PEM.lines().collect();
  let first = lines.first().expect("the fixture PEM has a first line");
  let last = lines.last().expect("the fixture PEM has a last line");
  let body = lines.get(1..lines.len() / 2).expect("half the body");
  let truncated = format!("{first}\n{}\n{last}\n", body.join("\n"));
  let err = build_app_jwt_at(&truncated, APP_ID, NOW).expect_err("a truncated key cannot sign");
  let message = err.to_string();
  assert!(
    message.starts_with("config error: the GitHub App private key is not a usable RSA PEM"),
    "{message}"
  );
  for line in body {
    assert!(
      !message.contains(line),
      "the error echoed key material: {message}"
    );
  }
}

/// A time whose `iat` or `exp` would overflow is an error, never a clamped
/// claim; the earliest instant that still fits signs.
#[test]
fn a_time_whose_claims_overflow_is_an_error_not_a_clamped_claim() {
  for now in [i64::MAX, i64::MAX - APP_JWT_LIFETIME_SECS + 1, i64::MIN] {
    let err = build_app_jwt_at(PRIVATE_PEM, APP_ID, now).expect_err("claims overflow");
    assert_eq!(
      err.to_string(),
      format!("protocol error: App JWT time {now} is out of range")
    );
  }
  let earliest = i64::MIN + APP_JWT_BACKDATE_SECS;
  let jwt = build_app_jwt_at(PRIVATE_PEM, APP_ID, earliest).expect("the earliest instant signs");
  let claims = verified_claims(&jwt).expect("signature verifies");
  assert_eq!(claims.get("iat").and_then(Value::as_i64), Some(i64::MIN));
}

#[test]
fn installation_token_parses_the_documented_reply_and_debug_redacts_the_token() {
  let token: InstallationToken =
    serde_json::from_str(&fixture_body(ACCESS_TOKENS_201).expect("fixture"))
      .expect("documented reply parses");
  assert_eq!(token.token, "ghs_EXAMPLE-installation-token-201");
  assert_eq!(token.expires_at, "2016-07-11T22:14:10Z");
  let debug = format!("{token:?}");
  assert!(
    !debug.contains(&token.token),
    "Debug leaked the token: {debug}"
  );
  assert!(debug.contains("token: \"<redacted>\""), "{debug}");
}

#[test]
fn installation_parses_the_documented_reply() {
  let installation: Installation =
    serde_json::from_str(&fixture_body(INSTALLATION_200).expect("fixture"))
      .expect("documented reply parses");
  assert_eq!(installation.id, 1);
  assert_eq!(
    installation.account.map(|account| account.login).as_deref(),
    Some("github")
  );
}

/// #202: the installation list item carries `repository_selection`, which
/// `create-app` prints as "all repositories" or a repo count.
#[test]
fn installation_list_items_carry_the_repository_selection() {
  let list: Vec<Installation> =
    serde_json::from_str(&fixture_body(INSTALLATIONS_200).expect("fixture"))
      .expect("documented reply parses");
  let first = list.first().expect("one installation");
  assert_eq!(first.repository_selection.as_deref(), Some("selected"));
}

/// #202: `GET /app` (documented example) parses into the identity `--import`
/// refreshes the saved App from.
#[test]
fn app_info_parses_the_documented_get_app_reply() {
  let app: AppInfo =
    serde_json::from_str(&fixture_body(APP_200).expect("fixture")).expect("documented reply");
  assert_eq!(app.id, 1);
  assert_eq!(app.slug, "octoapp");
  assert_eq!(app.name, "Octocat App");
  assert_eq!(app.client_id, "Iv1.ab1112223334445c");
  assert_eq!(app.html_url, "https://github.com/apps/octoapp");
  let owner = app.owner.expect("documented owner");
  assert_eq!((owner.login.as_str(), owner.id), ("octocat", 1));
}

/// #202: the live `GET /orgs/CodaSignal` capture parses into the org's login
/// and id (the install link's `target_id`).
#[test]
fn org_info_parses_the_live_get_org_capture() {
  let org: OrgInfo =
    serde_json::from_str(&fixture_body(ORGS_200).expect("fixture")).expect("live capture");
  assert_eq!(org.login, "CodaSignal");
  assert_eq!(org.id, 212_445_385);
}

/// Unix seconds now, from `std` alone.
fn unix_now() -> Result<i64, Box<dyn std::error::Error>> {
  let secs = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)?
    .as_secs();
  Ok(i64::try_from(secs)?)
}
