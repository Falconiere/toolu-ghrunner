//! GitHub App manifest onboarding flow — strictly sync, no I/O, no network.
//!
//! The runner onboards itself as a GitHub App via the manifest flow: it POSTs
//! an [`AppManifest`] to its [`ManifestTarget`]'s form (`settings/apps/new`
//! for a personal account, `organizations/{org}/settings/apps/new` for an
//! organization), GitHub redirects back to a
//! loopback callback with a temporary `code`, and the caller exchanges that
//! code for the minted app credentials ([`ConversionResponse`]). This module
//! owns the pure pieces: manifest construction, response parsing, CSRF state,
//! the auto-submitting HTML form, and callback query parsing — plus the
//! headless relay's pieces (#205: [`relay_state`], [`relay_link`],
//! [`parse_pasted`]). The async HTTP exchange lives in `wire::net`.

use std::collections::BTreeMap;

use std::fmt::Write as _;

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use shared::RunnerError;

/// The GitHub web origin the manifest forms live under (github.com only).
const GITHUB_WEB: &str = "https://github.com";

/// Which account the App is created for: decides the manifest form and the
/// permissions (#202).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestTarget {
  /// The signed-in user's account: repository runners need the repository
  /// permission `administration: write`.
  Personal,
  /// An organization (by login): organization runners need
  /// `organization_self_hosted_runners: write`, and GitHub needs no
  /// Administration permission at that scope.
  Org(String),
}

impl ManifestTarget {
  /// The form the manifest is sent to (an HTTP `POST`):
  /// `https://github.com/settings/apps/new` or
  /// `https://github.com/organizations/{org}/settings/apps/new`.
  pub fn form_action(&self) -> String {
    match self {
      Self::Personal => format!("{GITHUB_WEB}/settings/apps/new"),
      Self::Org(org) => format!("{GITHUB_WEB}/organizations/{org}/settings/apps/new"),
    }
  }

  /// The manifest's `default_permissions` for this target.
  pub fn default_permissions(&self) -> BTreeMap<String, String> {
    let key = match self {
      Self::Personal => "administration",
      Self::Org(_) => "organization_self_hosted_runners",
    };
    BTreeMap::from([(key.to_owned(), "write".to_owned())])
  }
}

/// GitHub App manifest posted to a [`ManifestTarget`]'s form (onboard-only
/// shape).
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
  /// Build the onboard-only manifest for `target`: its permissions
  /// ([`ManifestTarget::default_permissions`]), `public=false`, no webhook.
  ///
  /// `url` is a homepage placeholder; `redirect_url` is the loopback callback
  /// the manifest flow returns to.
  pub fn for_target(name: &str, redirect_url: &str, target: &ManifestTarget) -> Self {
    Self {
      name: name.to_owned(),
      url: "https://github.com/toolu/toolu-runner".to_owned(),
      redirect_url: redirect_url.to_owned(),
      public: false,
      default_permissions: target.default_permissions(),
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
  /// The owner's numeric account id (the install link's `target_id`);
  /// `None` when GitHub omits it.
  #[serde(default)]
  pub id: Option<i64>,
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
      _ => {},
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

/// HTML-attribute-escape a string: `&`, `"`, `'`, `<`, `>`. `&` must go first
/// so the other replacements are not double-escaped. Both quote styles are
/// escaped so the result is safe in single- or double-quoted attributes.
fn html_attr_escape(input: &str) -> String {
  input
    .replace('&', "&amp;")
    .replace('"', "&quot;")
    .replace('\'', "&#39;")
    .replace('<', "&lt;")
    .replace('>', "&gt;")
}

/// The relay's `redirect_url`: `<relay_base>/gh/callback` (the get.toolu.sh
/// contract, CodaSignal/toolu.sh#88).
pub fn relay_callback_url(relay_base: &str) -> String {
  format!("{}/gh/callback", relay_base.trim_end_matches('/'))
}

/// The `state` of a headless round trip: the lowercase hex SHA-256 of the
/// secret only the box keeps. The link, GitHub and the relay see only this
/// hash; claiming the code takes the secret itself.
pub fn relay_state(secret: &str) -> String {
  use sha2::Digest as _;
  let digest = sha2::Sha256::digest(secret.as_bytes());
  let mut hex = String::with_capacity(64);
  for byte in digest {
    // Writing to a `String` cannot fail.
    let _ = write!(hex, "{byte:02x}");
  }
  hex
}

/// The fragment payload the relay's form page reads (never sent to a server).
#[derive(Serialize)]
struct RelayPayload<'a> {
  manifest: &'a AppManifest,
  target: &'static str,
  #[serde(skip_serializing_if = "Option::is_none")]
  org: Option<&'a str>,
  state: &'a str,
}

/// The link the user opens on another machine:
/// `<relay_base>/gh/app#<base64url {manifest, target: "user"|"org", org, state}>`.
/// The relay's static page builds the GitHub form from the fragment.
///
/// # Errors
///
/// Returns `RunnerError::Protocol` on serialize failure.
pub fn relay_link(
  relay_base: &str,
  manifest: &AppManifest,
  target: &ManifestTarget,
  state: &str,
) -> Result<String, RunnerError> {
  let (kind, org) = match target {
    ManifestTarget::Personal => ("user", None),
    ManifestTarget::Org(org) => ("org", Some(org.as_str())),
  };
  let payload = RelayPayload {
    manifest,
    target: kind,
    org,
    state,
  };
  let json = serde_json::to_vec(&payload)
    .map_err(|err| RunnerError::Protocol(format!("relay link serialize failed: {err}")))?;
  Ok(format!(
    "{}/gh/app#{}",
    relay_base.trim_end_matches('/'),
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json)
  ))
}

/// Whether `code` can be a manifest code: 1–512 ASCII letters, digits, `-`
/// or `_`. It lands in the `/app-manifests/{code}/conversions` path, so a
/// code from the relay or a paste is checked before it is used.
pub fn is_manifest_code(code: &str) -> bool {
  (1..=512).contains(&code.len())
    && code
      .bytes()
      .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// What a pasted line holds.
#[derive(Debug, PartialEq, Eq)]
pub enum Pasted {
  /// A usable code (from a URL whose `state` is this run's, or a bare code).
  Code(String),
  /// A URL whose `state` belongs to another run: nothing may be exchanged.
  WrongState,
  /// Neither a usable URL nor (where allowed) a bare code.
  Invalid,
}

/// Read a pasted line: an address-bar URL `…?code=…&state=…` (its `state`
/// must be `expected_state`) or, unless `need_url`, the bare code the
/// relay's callback page shows.
pub fn parse_pasted(input: &str, expected_state: &str, need_url: bool) -> Pasted {
  let input = input.trim();
  let Some((_, query)) = input.split_once('?') else {
    return if !need_url && is_manifest_code(input) {
      Pasted::Code(input.to_owned())
    } else {
      Pasted::Invalid
    };
  };
  let query = query.split_once('#').map_or(query, |(query, _)| query);
  let (mut code, mut state) = (None, None);
  for pair in query.split('&') {
    match pair.split_once('=') {
      Some(("code", value)) => code = Some(value),
      Some(("state", value)) => state = Some(value),
      _ => {},
    }
  }
  match (code, state) {
    (Some(_), Some(state)) if state != expected_state => Pasted::WrongState,
    (Some(code), Some(_)) if is_manifest_code(code) => Pasted::Code(code.to_owned()),
    _ => Pasted::Invalid,
  }
}
