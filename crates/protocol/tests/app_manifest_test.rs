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

// ── #205: the headless relay (get.toolu.sh, CodaSignal/toolu.sh#88) ──

use base64::Engine as _;
use protocol::app_manifest::{
  Pasted, is_manifest_code, new_relay_secret, parse_pasted, relay_callback_url, relay_link,
  relay_state,
};

/// A relay secret is 64 lowercase hex characters and fresh on every call.
#[test]
fn relay_secrets_are_fresh_hex() {
  let (a, b) = (new_relay_secret(), new_relay_secret());
  assert_eq!(a.len(), 64);
  assert!(
    a.bytes()
      .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
    "{a}"
  );
  assert_ne!(a, b);
}

/// AC-6: `state` is the lowercase hex SHA-256 of the secret (FIPS 180-2).
#[test]
fn relay_state_is_the_sha256_hex_of_the_secret() {
  assert_eq!(
    relay_state("abc"),
    "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
  );
}

/// Decode the fragment of a relay link printed for `base`.
fn fragment(link: &str, base: &str) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
  let encoded = link
    .strip_prefix(&format!("{base}/gh/app#"))
    .ok_or("link is not <base>/gh/app#<fragment>")?;
  let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(encoded)?;
  Ok(serde_json::from_slice(&bytes)?)
}

/// AC-6: an org link carries the manifest as an object, `target: "org"`,
/// the org login and the state; the redirect is the relay's callback.
#[test]
fn org_relay_link_carries_manifest_target_org_and_state() {
  let base = "https://get.toolu.sh";
  let target = ManifestTarget::Org("acme".to_owned());
  let manifest = AppManifest::for_target("toolu-runner-acme", &relay_callback_url(base), &target);
  let state = relay_state("secret");
  let link = relay_link(&format!("{base}/"), &manifest, &target, &state).expect("link");
  let payload = fragment(&link, base).expect("fragment decodes");
  assert_eq!(payload.pointer("/target"), Some(&serde_json::json!("org")));
  assert_eq!(payload.pointer("/org"), Some(&serde_json::json!("acme")));
  assert_eq!(
    payload.pointer("/state"),
    Some(&serde_json::json!(state.as_str()))
  );
  assert_eq!(
    payload.pointer("/manifest/redirect_url"),
    Some(&serde_json::json!("https://get.toolu.sh/gh/callback"))
  );
  assert_eq!(
    payload.pointer("/manifest/default_permissions"),
    Some(&serde_json::json!({"organization_self_hosted_runners": "write"}))
  );
}

/// AC-6: a personal link says `target: "user"` and carries no `org` key.
#[test]
fn personal_relay_link_has_user_target_and_no_org() {
  let base = "http://127.0.0.1:9";
  let manifest = AppManifest::for_target(
    "toolu-runner-box",
    "http://127.0.0.1:50123/cb",
    &ManifestTarget::Personal,
  );
  let link = relay_link(base, &manifest, &ManifestTarget::Personal, "ab").expect("link");
  let payload = fragment(&link, base).expect("fragment decodes");
  assert_eq!(payload.pointer("/target"), Some(&serde_json::json!("user")));
  assert!(payload.get("org").is_none(), "{payload}");
  assert_eq!(
    payload.pointer("/manifest/redirect_url"),
    Some(&serde_json::json!("http://127.0.0.1:50123/cb"))
  );
}

/// A manifest code is one URL-path-safe token: nothing that could leave the
/// `/app-manifests/{code}/conversions` path segment.
#[test]
fn manifest_codes_are_path_safe_tokens() {
  assert!(is_manifest_code("9f3c2e0a71e"));
  assert!(is_manifest_code("a-b_C9"));
  for bad in ["", "../app", "a/b", "a b", "a?b", "a%2e", &"x".repeat(513)] {
    assert!(!is_manifest_code(bad), "{bad:?} accepted");
  }
}

/// S3: a pasted address-bar URL yields its code only when its `state` is
/// this run's; a wrong state is told apart from garbage.
#[test]
fn pasted_urls_are_bound_to_this_runs_state() {
  let state = relay_state("s");
  let good = format!("http://127.0.0.1:50123/cb?code=c0de&state={state}");
  assert_eq!(
    parse_pasted(&good, &state, true),
    Pasted::Code("c0de".to_owned())
  );
  assert_eq!(
    parse_pasted(&format!("  {good}#frag \n"), &state, true),
    Pasted::Code("c0de".to_owned())
  );
  let other = relay_state("another run");
  let wrong = format!("http://127.0.0.1:50123/cb?code=c0de&state={other}");
  assert_eq!(parse_pasted(&wrong, &state, true), Pasted::WrongState);
  assert_eq!(
    parse_pasted("http://127.0.0.1:1/cb?code=c0de", &state, true),
    Pasted::Invalid
  );
  let unsafe_code = format!("http://x/cb?code=../x&state={state}");
  assert_eq!(parse_pasted(&unsafe_code, &state, true), Pasted::Invalid);
}

/// A bare code (the relay callback page's) is accepted only when no URL is
/// required (`--no-relay` needs the state, so the URL).
#[test]
fn bare_codes_need_the_relay_mode() {
  let state = relay_state("s");
  assert_eq!(
    parse_pasted(" c0de\n", &state, false),
    Pasted::Code("c0de".to_owned())
  );
  assert_eq!(parse_pasted("c0de", &state, true), Pasted::Invalid);
  assert_eq!(parse_pasted("not a code", &state, false), Pasted::Invalid);
}
