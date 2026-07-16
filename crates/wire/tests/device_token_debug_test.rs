//! Finding 2 regression: `DeviceToken`'s `Debug` must never print the
//! bearer `access_token` in cleartext. It is a latent leak the moment the
//! value is `{:?}`-logged — directly, or via `PollOutcome::Token(..)` whose
//! derived `Debug` delegates to this impl. Mirrors `protocol::AccessToken`.

use wire::net::device_auth::{DeviceToken, PollOutcome, parse_poll_response};

/// The bearer value that must never surface in `Debug` output.
const SECRET: &str = "gho_16C7e42F292c6912E7710c838347Ae178B4a";

/// A realistic GitHub device-flow success body, as returned by
/// `POST /login/oauth/access_token` with `Accept: application/json`.
const SUCCESS_BODY: &str =
  r#"{"access_token":"gho_16C7e42F292c6912E7710c838347Ae178B4a","token_type":"bearer","scope":"repo,workflow"}"#;

#[test]
fn device_token_debug_redacts_access_token() -> Result<(), Box<dyn std::error::Error>> {
  let PollOutcome::Token(token) = parse_poll_response(SUCCESS_BODY) else {
    return Err("expected a token outcome from the success body".into());
  };

  // The token parsed correctly off the real wire shape...
  assert_eq!(token.access_token, SECRET);
  assert_eq!(token.token_type, "bearer");
  assert_eq!(token.scope, "repo,workflow");

  // ...but its Debug rendering must not leak the bearer.
  let rendered = format!("{token:?}");
  assert!(
    !rendered.contains(SECRET),
    "DeviceToken Debug leaked the access_token: {rendered}"
  );
  assert!(
    rendered.contains("<redacted>"),
    "DeviceToken Debug should mark access_token redacted: {rendered}"
  );
  // Non-secret fields stay visible so failures remain diagnosable.
  assert!(
    rendered.contains("bearer"),
    "token_type should remain in Debug: {rendered}"
  );
  assert!(
    rendered.contains("repo,workflow"),
    "scope should remain in Debug: {rendered}"
  );
  Ok(())
}

#[test]
fn poll_outcome_token_debug_inherits_redaction() {
  let outcome = PollOutcome::Token(DeviceToken {
    access_token: SECRET.to_owned(),
    token_type: "bearer".to_owned(),
    scope: "repo".to_owned(),
  });

  let rendered = format!("{outcome:?}");
  assert!(
    !rendered.contains(SECRET),
    "PollOutcome::Token Debug leaked the access_token: {rendered}"
  );
  assert!(
    rendered.contains("<redacted>"),
    "PollOutcome::Token Debug should mark access_token redacted: {rendered}"
  );
}
