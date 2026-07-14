//! Real-socket tests for the GitHub App manifest round trip.
//!
//! No HTTP mock: [`CallbackServer`] binds a genuine loopback listener and a
//! real `reqwest` client drives the two GETs GitHub's browser flow makes —
//! `GET /` (the auto-submit form) and the `GET /callback?code=…&state=…`
//! redirect. The CSRF check and the pure error/URL helpers are pinned too.

use std::time::Duration;

use protocol::app_manifest::AppManifest;
use tokio::io::AsyncWriteExt;
use wire::net::app_manifest::{CallbackServer, build_conversion_url, map_conversion_error};

/// Boxed error alias so test bodies can use `?` (the workspace lints deny
/// `unwrap`/`expect`).
type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

/// Build the onboard manifest JSON the served form embeds, with `redirect_url`
/// pointed at the bound loopback callback.
fn manifest_json(redirect_url: &str) -> TestResult<String> {
  Ok(AppManifest::for_runner("t", redirect_url).to_json()?)
}

/// AC-4 + AC-5 (happy): the form is served with the CSRF state in its action,
/// and a matching-state callback yields the code.
#[tokio::test]
async fn form_served_and_matching_callback_yields_code() -> TestResult<()> {
  let server = CallbackServer::bind("STATE123".to_owned()).await?;
  let base = server.local_url();
  let base = base.trim_end_matches('/').to_owned();
  // manifest is built after bind() so its redirect_url carries the bound port.
  let manifest = manifest_json(&server.callback_url())?;
  let handle = tokio::spawn(server.wait_for_code(manifest, Duration::from_secs(10)));

  let client = reqwest::Client::new();

  // GET / — the auto-submitting manifest form.
  let form = client.get(format!("{base}/")).send().await?.text().await?;
  assert!(
    form.contains("action=\"https://github.com/settings/apps/new?state=STATE123\""),
    "form action missing/wrong: {form}"
  );
  // The manifest JSON is HTML-escaped inside the hidden input's `value="…"`
  // (html_attr_escape turns every `"` into `&quot;`, so the value carries no
  // raw quote); scope the permission check to that attribute so it can't match
  // in the form action URL or elsewhere.
  let manifest_value = form
    .split_once("name=\"manifest\" value=\"")
    .and_then(|(_, rest)| rest.split_once('"'))
    .map(|(value, _)| value)
    .expect("manifest input with value attribute");
  assert!(
    manifest_value.contains("&quot;administration&quot;:&quot;write&quot;"),
    "administration:write permission missing from manifest value: {manifest_value}"
  );

  // GET /callback — GitHub's post-creation redirect.
  let done = client
    .get(format!("{base}/callback?code=abc123&state=STATE123"))
    .send()
    .await?;
  assert!(
    done.status().is_success(),
    "callback status: {}",
    done.status()
  );

  let code = handle.await??;
  assert_eq!(code, "abc123");
  Ok(())
}

/// Blocker fix: a spurious/aborted connection (a probe that connects, writes a
/// partial request line, then drops without a newline) must NOT end the flow —
/// the real callback that follows still yields the code.
#[tokio::test]
async fn spurious_connection_does_not_abort_the_flow() -> TestResult<()> {
  let server = CallbackServer::bind("STATE123".to_owned()).await?;
  let base = server.local_url();
  let base = base.trim_end_matches('/').to_owned();
  let authority = base
    .split_once("://")
    .map(|(_, a)| a)
    .unwrap_or(base.as_str())
    .to_owned();
  let manifest = manifest_json(&server.callback_url())?;
  let handle = tokio::spawn(server.wait_for_code(manifest, Duration::from_secs(10)));

  // Two spurious connections that must NOT terminate the accept loop: a
  // partial (newline-less) line that drops mid-request, and a complete but
  // unexpected request — the favicon/preconnect a real browser fires.
  {
    let mut probe = tokio::net::TcpStream::connect(&authority).await?;
    probe.write_all(b"GET /fav").await?;
    probe.flush().await?;
  }
  {
    let mut probe = tokio::net::TcpStream::connect(&authority).await?;
    probe
      .write_all(b"GET /favicon.ico HTTP/1.1\r\nHost: localhost\r\n\r\n")
      .await?;
    probe.flush().await?;
  }

  let client = reqwest::Client::new();
  let done = client
    .get(format!("{base}/callback?code=abc123&state=STATE123"))
    .send()
    .await?;
  assert!(
    done.status().is_success(),
    "callback status: {}",
    done.status()
  );

  let code = handle.await??;
  assert_eq!(code, "abc123");
  Ok(())
}

/// AC-5 (CSRF): a callback whose state does not match is rejected with a `400`
/// and `wait_for_code` returns an error.
#[tokio::test]
async fn callback_with_wrong_state_is_rejected() -> TestResult<()> {
  let server = CallbackServer::bind("STATE123".to_owned()).await?;
  let base = server.local_url();
  let base = base.trim_end_matches('/').to_owned();
  let manifest = manifest_json(&server.callback_url())?;
  let handle = tokio::spawn(server.wait_for_code(manifest, Duration::from_secs(10)));

  let client = reqwest::Client::new();
  let resp = client
    .get(format!("{base}/callback?code=abc&state=WRONG"))
    .send()
    .await?;
  assert_eq!(
    resp.status().as_u16(),
    400,
    "expected 400 for CSRF mismatch"
  );

  let inner = handle.await?;
  assert!(inner.is_err(), "expected CSRF rejection, got: {inner:?}");
  Ok(())
}

/// AC-6: a spent/invalid code (404/422) names `create-app`, and the dotcom
/// conversion URL targets `api.github.com`.
#[test]
fn spent_code_errors_name_create_app_and_dotcom_url_is_api() {
  let e422 = map_conversion_error(422, "{\"message\":\"Not Found\"}");
  assert_eq!(
    e422.to_string(),
    "auth error: GitHub rejected the app manifest code (HTTP 422) \u{2014} the temporary code is \
     single-use and has been spent or is invalid; re-run `create-app` to generate a fresh \
     manifest and code"
  );
  let e404 = map_conversion_error(404, "{\"message\":\"Not Found\"}");
  assert_eq!(
    e404.to_string(),
    "auth error: GitHub rejected the app manifest code (HTTP 404) \u{2014} the temporary code is \
     single-use and has been spent or is invalid; re-run `create-app` to generate a fresh \
     manifest and code"
  );

  assert_eq!(
    build_conversion_url("github.com", "XYZ"),
    "https://api.github.com/app-manifests/XYZ/conversions"
  );
}
