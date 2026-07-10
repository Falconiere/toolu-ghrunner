//! AC-7: `parse_poll_response` over real GitHub device-flow bodies.
//!
//! Every string below is a documented wire shape GitHub returns from
//! `POST https://github.com/login/oauth/access_token` during the device
//! flow — real payloads, not mocks. The classifier is pure, so no HTTP
//! (and no client id) is needed to exercise it.

use toolu_runner::net::device_auth::{PollOutcome, parse_poll_response};

#[test]
fn authorization_pending_maps_to_pending() {
  let body = r#"{"error":"authorization_pending","error_description":"The authorization request is still pending.","error_uri":"https://docs.github.com/developers/apps/authorizing-oauth-apps#error-codes-for-the-device-flow"}"#;
  assert!(matches!(parse_poll_response(body), PollOutcome::Pending));
}

#[test]
fn slow_down_maps_to_slow_down() {
  let body = r#"{"error":"slow_down","error_description":"You have polled too fast. Wait a few seconds and try again.","interval":10,"error_uri":"https://docs.github.com/developers/apps/authorizing-oauth-apps#error-codes-for-the-device-flow"}"#;
  assert!(matches!(parse_poll_response(body), PollOutcome::SlowDown));
}

#[test]
fn expired_token_maps_to_expired() {
  let body = r#"{"error":"expired_token","error_description":"The device code has expired. Please run the login command again.","error_uri":"https://docs.github.com/developers/apps/authorizing-oauth-apps#error-codes-for-the-device-flow"}"#;
  assert!(matches!(parse_poll_response(body), PollOutcome::Expired));
}

#[test]
fn access_denied_maps_to_denied() {
  let body = r#"{"error":"access_denied","error_description":"The authorization request was denied.","error_uri":"https://docs.github.com/developers/apps/authorizing-oauth-apps#error-codes-for-the-device-flow"}"#;
  assert!(matches!(parse_poll_response(body), PollOutcome::Denied));
}

#[test]
fn success_body_maps_to_token() {
  let body = r#"{"access_token":"gho_REALSHAPE","token_type":"bearer","scope":"repo,admin:org"}"#;
  let outcome = parse_poll_response(body);
  assert!(
    matches!(&outcome, PollOutcome::Token(_)),
    "success body should classify as Token; got {outcome:?}"
  );
  if let PollOutcome::Token(token) = outcome {
    assert_eq!(token.access_token, "gho_REALSHAPE");
    assert_eq!(token.token_type, "bearer");
    assert_eq!(token.scope, "repo,admin:org");
  }
}

#[test]
fn unrecognized_error_carries_the_error_code() {
  let body = r#"{"error":"device_flow_disabled","error_description":"Device flow is not enabled on this OAuth app.","error_uri":"https://docs.github.com/developers/apps/authorizing-oauth-apps#error-codes-for-the-device-flow"}"#;
  let outcome = parse_poll_response(body);
  assert!(
    matches!(&outcome, PollOutcome::Error(code) if code == "device_flow_disabled"),
    "unknown error should surface its code verbatim; got {outcome:?}"
  );
}
