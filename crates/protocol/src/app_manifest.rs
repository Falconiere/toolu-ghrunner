//! GitHub App manifest onboarding flow — strictly sync, no I/O, no network.
//!
//! The runner onboards itself as a GitHub App via the manifest flow: it POSTs
//! an [`AppManifest`] to `settings/apps/new`, GitHub redirects back to a
//! loopback callback with a temporary `code`, and the caller exchanges that
//! code for the minted app credentials ([`ConversionResponse`]). This module
//! owns the pure pieces: manifest construction, response parsing, CSRF state,
//! the auto-submitting HTML form, and callback query parsing. The async HTTP
//! exchange lives in `wire::net`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use shared::RunnerError;

/// GitHub App manifest posted to `settings/apps/new` (onboard-only shape).
///
/// Serializes (in declaration order) to exactly `{name, url, redirect_url,
/// public, default_permissions, default_events}` — deliberately no
/// `hook_attributes`, so the minted app has no webhook.
#[derive(Debug, Clone, Serialize)]
pub struct AppManifest {
  name: String,
  url: String,
  redirect_url: String,
  public: bool,
  default_permissions: BTreeMap<String, String>,
  default_events: Vec<String>,
}

impl AppManifest {
  /// Build the onboard-only manifest: `administration:write`, `public=false`,
  /// no webhook.
  ///
  /// `url` is a homepage placeholder; `redirect_url` is the loopback callback
  /// the manifest flow returns to.
  pub fn for_runner(name: &str, redirect_url: &str) -> Self {
    let mut default_permissions = BTreeMap::new();
    default_permissions.insert("administration".to_owned(), "write".to_owned());

    Self {
      name: name.to_owned(),
      url: "https://github.com/toolu/toolu-runner".to_owned(),
      redirect_url: redirect_url.to_owned(),
      public: false,
      default_permissions,
      default_events: Vec::new(),
    }
  }

  /// Serialize to the JSON GitHub expects.
  ///
  /// # Errors
  ///
  /// Returns `RunnerError::Protocol` on serialize failure.
  pub fn to_json(&self) -> Result<String, RunnerError> {
    serde_json::to_string(self)
      .map_err(|err| RunnerError::Protocol(format!("app manifest serialize failed: {err}")))
  }
}

/// Owner (account) that the minted GitHub App belongs to.
#[derive(Debug, Clone, Deserialize)]
pub struct ConversionOwner {
  /// The owner's login handle.
  pub login: String,
}

/// Response body from `POST /app-manifests/{code}/conversions`.
///
/// Carries the minted app's identity plus its secrets. `webhook_secret` is
/// `null` when the manifest declared no webhook (our onboard-only shape).
#[derive(Debug, Clone, Deserialize)]
pub struct ConversionResponse {
  /// Numeric GitHub App id.
  pub id: i64,
  /// URL-friendly app slug.
  pub slug: String,
  /// GraphQL node id.
  pub node_id: String,
  /// Owning account.
  pub owner: ConversionOwner,
  /// Human-readable app name.
  pub name: String,
  /// OAuth client id.
  pub client_id: String,
  /// OAuth client secret.
  pub client_secret: String,
  /// Webhook signing secret; `None` when the app has no webhook.
  pub webhook_secret: Option<String>,
  /// PEM-encoded RSA private key for signing app JWTs.
  pub pem: String,
  /// The app's settings page URL.
  pub html_url: String,
}

/// Parse the `POST /app-manifests/{code}/conversions` response body.
///
/// # Errors
///
/// Returns `RunnerError::Protocol` on JSON parse failure.
pub fn parse_conversion(body: &str) -> Result<ConversionResponse, RunnerError> {
  serde_json::from_str(body)
    .map_err(|err| RunnerError::Protocol(format!("app conversion response parse failed: {err}")))
}

/// New CSRF state token (uuid v4 string) for the manifest round trip.
pub fn new_state() -> String {
  uuid::Uuid::new_v4().to_string()
}

/// The auto-submitting HTML form served at `GET /` of the loopback callback
/// server.
///
/// Renders a full page whose form POSTs the manifest to
/// `{action_base}?state={state}` and submits itself on load. The manifest JSON
/// and the `state` are HTML-attribute-escaped before interpolation.
pub fn form_html(manifest_json: &str, state: &str, action_base: &str) -> String {
  let manifest_attr = html_attr_escape(manifest_json);
  let state = html_attr_escape(state);
  format!(
    "<!DOCTYPE html>\n\
<html lang=\"en\">\n\
<head>\n\
<meta charset=\"utf-8\">\n\
<title>Creating the toolu-runner GitHub App\u{2026}</title>\n\
</head>\n\
<body>\n\
<p>Redirecting to GitHub to create the runner's GitHub App\u{2026}</p>\n\
<form id=\"manifest-form\" method=\"post\" action=\"{action_base}?state={state}\">\n\
<input type=\"hidden\" name=\"manifest\" value=\"{manifest_attr}\">\n\
<noscript><button type=\"submit\">Continue to GitHub</button></noscript>\n\
</form>\n\
<script>document.getElementById('manifest-form').submit();</script>\n\
</body>\n\
</html>\n"
  )
}

/// Parse `GET /callback` path+query, verify the CSRF state, return the `code`.
///
/// Input looks like `"/callback?code=abc123&state=xyz"`. The query is
/// hand-split (`?`, then `&`, then `=`); values are alphanumeric uuid/code
/// tokens so no percent-decoding is needed.
///
/// # Errors
///
/// Returns `RunnerError::Protocol` on a state mismatch (possible CSRF) or a
/// missing `code`.
pub fn parse_callback_path(
  path_and_query: &str,
  expected_state: &str,
) -> Result<String, RunnerError> {
  let query = path_and_query.split_once('?').map_or("", |(_, q)| q);

  let mut code: Option<String> = None;
  let mut state: Option<String> = None;
  for pair in query.split('&').filter(|p| !p.is_empty()) {
    let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
    match key {
      "code" => code = Some(value.to_owned()),
      "state" => state = Some(value.to_owned()),
      _ => {}
    }
  }

  let state = state.ok_or_else(|| {
    RunnerError::Protocol("manifest callback missing state param (possible CSRF)".to_owned())
  })?;
  if state != expected_state {
    return Err(RunnerError::Protocol(
      "manifest callback CSRF state mismatch".to_owned(),
    ));
  }

  code.ok_or_else(|| RunnerError::Protocol("manifest callback missing code param".to_owned()))
}

/// HTML-attribute-escape a string: `&`, `"`, `<`, `>`. `&` must go first so
/// the other replacements are not double-escaped.
fn html_attr_escape(input: &str) -> String {
  input
    .replace('&', "&amp;")
    .replace('"', "&quot;")
    .replace('<', "&lt;")
    .replace('>', "&gt;")
}
