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
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn form_served_and_matching_callback_yields_code() -> TestResult<()> {
  let server = CallbackServer::bind("STATE123".to_owned()).await?;
  let base = server.local_url();
  let base = base.trim_end_matches('/').to_owned();
  // The manifest's redirect_url embeds the bound port, so build it post-bind.
  let manifest = manifest_json(&server.callback_url())?;
  let handle = tokio::spawn(server.wait_for_code(manifest, Duration::from_secs(10)));

  let client = reqwest::Client::new();

  // GET / — the auto-submitting manifest form.
  let form = client.get(format!("{base}/")).send().await?.text().await?;
  assert!(
    form.contains("action=\"https://github.com/settings/apps/new?state=STATE123\""),
    "form action missing/wrong: {form}"
  );
  assert!(
    form.contains("name=\"manifest\""),
    "manifest input missing: {form}"
  );
  assert!(
    form.contains("administration"),
    "manifest permission missing: {form}"
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
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spurious_connection_does_not_abort_the_flow() -> TestResult<()> {
  let server = CallbackServer::bind("STATE123".to_owned()).await?;
  let base = server.local_url();
  let base = base.trim_end_matches('/').to_owned();
  let authority = base.trim_start_matches("http://").to_owned();
  let manifest = manifest_json(&server.callback_url())?;
  let handle = tokio::spawn(server.wait_for_code(manifest, Duration::from_secs(10)));

  // A probe that connects, writes a partial (newline-less) request line, then
  // drops at scope end — the kind of favicon/preconnect/aborted request that
  // must not terminate the accept loop.
  {
    let mut probe = tokio::net::TcpStream::connect(&authority).await?;
    probe.write_all(b"GET /fav").await?;
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
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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
  assert!(
    e422.to_string().contains("create-app"),
    "422 message missing create-app hint: {e422}"
  );
  let e404 = map_conversion_error(404, "{\"message\":\"Not Found\"}");
  assert!(
    e404.to_string().contains("create-app"),
    "404 message missing create-app hint: {e404}"
  );

  assert_eq!(
    build_conversion_url("github.com", "XYZ"),
    "https://api.github.com/app-manifests/XYZ/conversions"
  );
}
