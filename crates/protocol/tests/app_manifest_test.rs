//! Real-data tests for the GitHub App manifest onboarding flow.
//!
//! No mocks: AC-1 round-trips the built manifest through serde_json, AC-2
//! parses a shape-real conversion-response fixture, and the remaining tests
//! exercise the HTML form and the CSRF/callback query parser. The fixture's
//! `pem` is a non-secret placeholder block (never used cryptographically — the
//! parser only deserializes the field), so no private key is committed.

use protocol::app_manifest::{
  AppManifest, form_html, new_state, parse_callback_path, parse_conversion,
};
use serde_json::Value;

/// AC-1: the built manifest serializes to the exact onboard-only shape, with
/// no `hook_attributes` key.
#[test]
fn for_runner_serializes_onboard_only_shape() {
  let json = AppManifest::for_runner("toolu-x", "http://127.0.0.1:5000/callback")
    .to_json()
    .expect("manifest serializes");
  let value: Value = serde_json::from_str(&json).expect("manifest is valid JSON");

  assert_eq!(
    value
      .pointer("/default_permissions/administration")
      .and_then(Value::as_str),
    Some("write"),
    "administration permission must be write"
  );
  assert_eq!(
    value.get("public").and_then(Value::as_bool),
    Some(false),
    "app must not be public"
  );
  assert_eq!(
    value.get("redirect_url").and_then(Value::as_str),
    Some("http://127.0.0.1:5000/callback"),
    "redirect_url must round-trip"
  );
  assert!(
    value.get("hook_attributes").is_none(),
    "onboard-only manifest must declare no webhook"
  );
}

/// AC-2: the conversion-response fixture parses into the typed shape.
#[test]
fn parse_conversion_reads_fixture() {
  let body = include_str!("fixtures/conversion_response.json");
  let resp = parse_conversion(body).expect("fixture parses");

  assert_eq!(resp.id, 123456);
  assert_eq!(resp.slug, "toolu-runner-test");
  assert_eq!(resp.owner.login, "octocat");
  assert_eq!(resp.client_id, "Iv1.testclientid0001");
  assert!(!resp.pem.is_empty(), "pem must be present");
  assert!(
    resp.pem.starts_with("-----BEGIN"),
    "pem must be a PEM block"
  );
}

/// The auto-submitting form carries the POST target, the hidden manifest
/// input, and the manifest's `administration` permission.
#[test]
fn form_html_contains_structural_markers() {
  let manifest_json = AppManifest::for_runner("toolu-x", "http://127.0.0.1:5000/callback")
    .to_json()
    .expect("manifest serializes");
  let html = form_html(
    &manifest_json,
    "STATE123",
    "https://github.com/settings/apps/new",
  );

  assert!(
    html.contains("action=\"https://github.com/settings/apps/new?state=STATE123\""),
    "form must POST to the state-scoped manifest endpoint"
  );
  assert!(
    html.contains("name=\"manifest\""),
    "form must carry the hidden manifest input"
  );
  assert!(
    html.contains("&quot;administration&quot;:&quot;write&quot;"),
    "HTML-attribute-escaped manifest permission must survive into the form value"
  );
}

/// A matching state returns the code; a mismatch or a missing code errors.
#[test]
fn parse_callback_verifies_state_and_returns_code() {
  assert_eq!(
    parse_callback_path("/callback?code=abc123&state=STATE123", "STATE123")
      .expect("matching state yields the code"),
    "abc123"
  );

  assert!(
    parse_callback_path("/callback?code=abc123&state=WRONG", "STATE123").is_err(),
    "CSRF state mismatch must error"
  );

  assert!(
    parse_callback_path("/callback?state=STATE123", "STATE123").is_err(),
    "a missing code must error"
  );
}

/// `new_state` produces distinct uuid-length tokens.
#[test]
fn new_state_is_a_fresh_token() {
  let a = new_state();
  let b = new_state();
  assert_ne!(a, b, "each state token must be fresh");
  assert_eq!(a.len(), 36, "state token is a hyphenated uuid v4");
}
