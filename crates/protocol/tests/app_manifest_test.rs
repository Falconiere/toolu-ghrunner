//! Real-data tests for the GitHub App manifest onboarding flow.
//!
//! No mocks: S1 (#202) pins the exact manifest JSON per target, AC-2
//! parses a shape-real conversion-response fixture, and the remaining tests
//! exercise the HTML form and the CSRF/callback query parser. The fixture's
//! `pem` is a non-secret placeholder block (never used cryptographically — the
//! parser only deserializes the field), so no private key is committed.

use protocol::app_manifest::{
  AppManifest, ManifestTarget, form_html, new_state, parse_callback_path, parse_conversion,
};

/// #202 S1: the personal manifest serializes to exactly the onboard-only
/// shape — `administration: write`, private, no webhook key.
#[test]
fn personal_manifest_serializes_to_the_exact_json() {
  let json = AppManifest::for_target(
    "toolu-runner-octocat",
    "http://127.0.0.1:5000/callback",
    &ManifestTarget::Personal,
  )
  .to_json()
  .expect("manifest serializes");
  assert_eq!(
    json,
    r#"{"name":"toolu-runner-octocat","url":"https://github.com/toolu/toolu-runner","redirect_url":"http://127.0.0.1:5000/callback","public":false,"default_permissions":{"administration":"write"},"default_events":[]}"#
  );
}

/// #202 S1: the org manifest asks for the organization Self-hosted runners
/// permission only (GitHub: Administration is not needed at org scope).
#[test]
fn org_manifest_serializes_to_the_exact_json() {
  let json = AppManifest::for_target(
    "toolu-runner-acme",
    "http://127.0.0.1:5000/callback",
    &ManifestTarget::Org("acme".to_owned()),
  )
  .to_json()
  .expect("manifest serializes");
  assert_eq!(
    json,
    r#"{"name":"toolu-runner-acme","url":"https://github.com/toolu/toolu-runner","redirect_url":"http://127.0.0.1:5000/callback","public":false,"default_permissions":{"organization_self_hosted_runners":"write"},"default_events":[]}"#
  );
}

/// #202 S1: each target posts its manifest to its own GitHub form.
#[test]
fn each_target_names_its_manifest_form() {
  assert_eq!(
    ManifestTarget::Personal.form_action(),
    "https://github.com/settings/apps/new"
  );
  assert_eq!(
    ManifestTarget::Org("acme".to_owned()).form_action(),
    "https://github.com/organizations/acme/settings/apps/new"
  );
}

/// AC-2: the conversion-response fixture parses into the typed shape.
#[test]
fn parse_conversion_reads_fixture() {
  let body = include_str!("fixtures/conversion_response.json");
  let resp = parse_conversion(body).expect("fixture parses");

  assert_eq!(resp.id, 123_456);
  assert_eq!(resp.slug, "toolu-runner-test");
  assert_eq!(resp.owner.login, "octocat");
  // A reply without `owner.id` (this fixture predates #202) still parses.
  assert_eq!(resp.owner.id, None);
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
  let manifest_json = AppManifest::for_target(
    "toolu-x",
    "http://127.0.0.1:5000/callback",
    &ManifestTarget::Personal,
  )
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
