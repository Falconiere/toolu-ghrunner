//! OIDC upstream-URL builder: the caller-supplied `audience` must be
//! percent-encoded so it cannot inject an extra query parameter or a URL
//! fragment into the request GitHub's OIDC provider receives.
//!
//! Closes a pre-push review gap: `oidc_request_url` / `percent_encode_query`
//! shipped untested. These assert the built URL directly (no mocks) — an
//! audience carrying `& = # /` lands as `%XX`, never as a live delimiter.

use execution::execution::oidc::oidc_request_url;

const UPSTREAM: &str = "https://token.actions.githubusercontent.com";
const BASE: &str =
  "https://token.actions.githubusercontent.com/_apis/pipeline/oidc/requestToken?api-version=1";

/// The substring of the built URL that carries the encoded audience value,
/// i.e. everything after the single `audience=` delimiter (empty string when
/// no audience is present — callers assert against the value they expect).
fn audience_value(url: &str) -> &str {
  url.split_once("audience=").map_or("", |(_, value)| value)
}

/// No audience → no `audience` param and no dangling `&`.
#[test]
fn no_audience_produces_bare_url() {
  let url = oidc_request_url(UPSTREAM, None);

  assert_eq!(url, BASE);
  assert!(!url.contains("audience"), "no audience param: {url}");
}

/// An audience packed with query/fragment delimiters is encoded byte-for-byte;
/// the extra `& = #` survive only as `%26 %3D %23`, never as live delimiters,
/// so a caller cannot smuggle in an extra query param or a fragment.
#[test]
fn query_and_fragment_chars_are_percent_encoded() {
  let url = oidc_request_url(UPSTREAM, Some("a&b=c#d"));

  // Exact identity of the built URL.
  assert_eq!(url, format!("{BASE}&audience=a%26b%3Dc%23d"));

  let value = audience_value(&url);
  assert_eq!(value, "a%26b%3Dc%23d");

  // The injected delimiters must NOT appear literally in the audience value.
  assert!(
    !value.contains('&'),
    "raw & would open a new query param: {value}"
  );
  assert!(
    !value.contains('='),
    "raw = would complete a key=value pair: {value}"
  );
  assert!(
    !value.contains('#'),
    "raw # would open a URL fragment: {value}"
  );

  // The literal injection sequences must be absent from the whole URL.
  assert!(
    !url.contains("a&b"),
    "audience & leaked as a delimiter: {url}"
  );
  assert!(
    !url.contains("b=c"),
    "audience = leaked as a delimiter: {url}"
  );
  assert!(
    !url.contains("c#d"),
    "audience # leaked as a fragment: {url}"
  );

  // And the encoded forms are present.
  assert!(url.contains("%26") && url.contains("%3D") && url.contains("%23"));
}

/// An audience that looks like a full URL cannot smuggle in path segments:
/// its `:` and `/` bytes are encoded, so the audience value carries no `/`.
#[test]
fn slashes_and_scheme_are_percent_encoded() {
  let url = oidc_request_url(UPSTREAM, Some("https://x/y"));

  assert_eq!(url, format!("{BASE}&audience=https%3A%2F%2Fx%2Fy"));

  let value = audience_value(&url);
  assert_eq!(value, "https%3A%2F%2Fx%2Fy");

  // No raw path separators or scheme colon in the audience value.
  assert!(
    !value.contains('/'),
    "raw / would inject a path segment: {value}"
  );
  assert!(
    !value.contains(':'),
    "raw : leaked from the audience: {value}"
  );
  assert!(value.contains("%2F") && value.contains("%3A"));
}

/// The unreserved RFC-3986 set passes through verbatim (not over-encoded), so a
/// normal audience like `sts.amazonaws.com` stays human-readable.
#[test]
fn unreserved_audience_passes_through_verbatim() {
  let url = oidc_request_url(UPSTREAM, Some("sts.amazonaws.com"));

  assert_eq!(url, format!("{BASE}&audience=sts.amazonaws.com"));
  assert_eq!(audience_value(&url), "sts.amazonaws.com");
}

/// A trailing slash on the upstream base is trimmed (no `//_apis`).
#[test]
fn trailing_slash_on_upstream_is_trimmed() {
  let url = oidc_request_url("https://token.actions.githubusercontent.com/", None);

  assert_eq!(url, BASE);
}
