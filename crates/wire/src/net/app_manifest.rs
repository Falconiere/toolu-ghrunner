//! GitHub App manifest onboarding — the async half of the manifest flow.
//!
//! The pure pieces (manifest JSON, the auto-submit form, CSRF state, callback
//! and response parsing) live in [`protocol::app_manifest`]. This module owns
//! the two I/O steps around them: [`CallbackServer`], a loopback HTTP server
//! that serves the form at `GET /` and captures GitHub's redirected
//! `GET /callback?code=…&state=…`, and [`convert_manifest_code`], which
//! exchanges that one-time code for the minted app credentials. Both run over
//! real sockets with no HTTP framework, keeping the round trip testable.
//! [`claim_relay_code`] is the headless path's (#205): it polls the
//! get.toolu.sh relay for the code GitHub's redirect left there.

use std::net::SocketAddr;
use std::time::Duration;

use protocol::app_manifest::{
  ConversionResponse, form_html, is_manifest_code, parse_callback_path, parse_conversion,
};
use shared::RunnerError;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

/// Upper bound on the request line we read before treating a connection as a
/// bad request (guards against a peer that never sends a newline).
const MAX_REQUEST_LINE: u64 = 8 * 1024;

/// The page shown in the browser once the callback code has been captured.
const SUCCESS_HTML: &str = "<!DOCTYPE html>\n\
<html lang=\"en\">\n\
<head><meta charset=\"utf-8\"><title>toolu-runner</title></head>\n\
<body>\n\
<p>GitHub App created \u{2014} return to your terminal.</p>\n\
</body>\n\
</html>\n";

/// Loopback server for the GitHub App Manifest browser round trip.
///
/// Bound to `127.0.0.1:0`, it holds the CSRF state to verify on the callback.
/// The manifest JSON is deferred to [`CallbackServer::wait_for_code`] because
/// its `redirect_url` embeds the bound port ([`Self::callback_url`]), which is
/// not known until [`Self::bind`].
pub struct CallbackServer {
  /// The bound loopback listener.
  listener: TcpListener,
  /// The CSRF state echoed in the form and verified on the callback.
  state: String,
  /// The bound address, captured once so the URL accessors are infallible.
  local_addr: SocketAddr,
}

impl CallbackServer {
  /// Bind `127.0.0.1:0`; hold the CSRF state to check on the callback.
  ///
  /// # Errors
  ///
  /// Returns `RunnerError::Network` when the loopback bind or address lookup
  /// fails.
  pub async fn bind(state: String) -> Result<Self, RunnerError> {
    let listener = TcpListener::bind("127.0.0.1:0")
      .await
      .map_err(|e| RunnerError::Network(format!("callback server bind failed: {e}")))?;
    let local_addr = listener
      .local_addr()
      .map_err(|e| RunnerError::Network(format!("callback server local_addr failed: {e}")))?;
    Ok(Self {
      listener,
      state,
      local_addr,
    })
  }

  /// `http://127.0.0.1:<port>/` — the URL to open in the browser.
  ///
  /// The IPv4 loopback literal is formatted explicitly (not via the
  /// `SocketAddr` `Display`), so the URL stays bracket-free and safe to embed
  /// in the manifest form even if the bind address ever changes.
  pub fn local_url(&self) -> String {
    format!("http://127.0.0.1:{}/", self.local_addr.port())
  }

  /// `http://127.0.0.1:<port>/callback` — the value the caller puts in the
  /// manifest's `redirect_url` so GitHub redirects back to this server.
  pub fn callback_url(&self) -> String {
    format!("http://127.0.0.1:{}/callback", self.local_addr.port())
  }

  /// Serve `GET /` (the auto-submit form that sends `manifest_json` to
  /// `form_action`, the target's `settings/apps/new` page) and
  /// `GET /callback` until a matching-state code arrives, or `timeout`
  /// elapses. Consumes `self`.
  ///
  /// # Errors
  ///
  /// Returns `RunnerError::Protocol` on a CSRF state mismatch or a missing
  /// code (from [`parse_callback_path`]), `RunnerError::Network` on a socket
  /// failure, and `RunnerError::Network` (browser-flow-timed-out message) when
  /// `timeout` elapses before a callback arrives.
  pub async fn wait_for_code(
    self,
    manifest_json: String,
    form_action: &str,
    timeout: Duration,
  ) -> Result<String, RunnerError> {
    let Self {
      listener, state, ..
    } = self;
    let form_page = form_html(&manifest_json, &state, form_action);
    let outcome = tokio::time::timeout(timeout, accept_loop(&listener, &form_page, &state)).await;
    match outcome {
      Ok(result) => result,
      Err(_elapsed) => Err(RunnerError::Network(
        "GitHub App manifest browser flow timed out before a callback arrived \u{2014} the \
         app may not have been created (a duplicate app name or a cancelled page blocks the \
         redirect); re-run `create-app` to try again"
          .to_owned(),
      )),
    }
  }
}

/// Accept connections until one carries a valid `/callback` code.
///
/// `GET /` serves the auto-submit form and keeps looping; `/callback` verifies
/// the CSRF state and returns the code (or its error); anything else 404s and
/// keeps looping.
async fn accept_loop(
  listener: &TcpListener,
  form_page: &str,
  state: &str,
) -> Result<String, RunnerError> {
  loop {
    let (mut stream, _addr) = listener
      .accept()
      .await
      .map_err(|e| RunnerError::Network(format!("callback accept failed: {e}")))?;
    match handle_connection(&mut stream, form_page, state).await {
      Ok(Some(code)) => return Ok(code),
      Ok(None) => {},
      Err(e) => return Err(e),
    }
  }
}

/// Serve one connection: thin dispatch over the request target.
///
/// Only a real `/callback` result is load-bearing: `Ok(Some(code))` ends the
/// flow and `Err` (a CSRF/parse rejection) terminates it. Every transport
/// hiccup on a non-callback path — a dropped/partial request, a favicon or
/// preconnect probe, a failed form/404 write — is logged and folded into
/// `Ok(None)` so the loop keeps serving until the browser's real callback
/// arrives.
async fn handle_connection(
  stream: &mut tokio::net::TcpStream,
  form_page: &str,
  state: &str,
) -> Result<Option<String>, RunnerError> {
  let (read_half, mut write_half) = stream.split();
  let Some(target) = read_target_or_log(read_half).await else {
    return Ok(None);
  };

  if target == "/" {
    log_write_err(serve_root(&mut write_half, form_page).await);
    return Ok(None);
  }
  if target.starts_with("/callback") {
    return serve_callback(&mut write_half, &target, state).await;
  }
  log_write_err(serve_not_found(&mut write_half).await);
  Ok(None)
}

/// Read the request target, folding a dropped/partial/over-cap line or a
/// transport read error into `None` (both are logged and mean "keep serving").
async fn read_target_or_log<R: tokio::io::AsyncRead + Unpin>(read_half: R) -> Option<String> {
  match read_request_target(read_half).await {
    Ok(Some(target)) => Some(target),
    Ok(None) => {
      tracing::debug!("manifest callback: incomplete or oversized request line, ignoring");
      None
    },
    Err(e) => {
      tracing::debug!(error = %e, "manifest callback: request read failed, ignoring");
      None
    },
  }
}

/// Write the pre-rendered auto-submit form page for `GET /`.
async fn serve_root<W: AsyncWriteExt + Unpin>(
  writer: &mut W,
  form_page: &str,
) -> Result<(), RunnerError> {
  write_http_response(
    writer,
    "HTTP/1.1 200 OK",
    "text/html; charset=utf-8",
    form_page,
  )
  .await
}

/// Write the `404` for an unrecognized path.
async fn serve_not_found<W: AsyncWriteExt + Unpin>(writer: &mut W) -> Result<(), RunnerError> {
  write_http_response(
    writer,
    "HTTP/1.1 404 Not Found",
    "text/plain; charset=utf-8",
    "Not found.\n",
  )
  .await
}

/// Log (never propagate) a best-effort non-callback write failure.
fn log_write_err(result: Result<(), RunnerError>) {
  if let Err(e) = result {
    tracing::debug!(error = %e, "manifest callback: response write failed, ignoring");
  }
}

/// Read the request line (capped at [`MAX_REQUEST_LINE`]) and return its target
/// (the second token of `GET <target> HTTP/1.1`).
///
/// `Ok(None)` means the peer sent an empty, partial, or over-cap line (dropped
/// connection or a client that never sends a newline) — the caller keeps
/// serving. `Err` is a genuine transport read failure.
async fn read_request_target<R: tokio::io::AsyncRead + Unpin>(
  read_half: R,
) -> Result<Option<String>, RunnerError> {
  let mut reader = BufReader::new(read_half.take(MAX_REQUEST_LINE));
  let mut request_line = String::new();
  let read = reader
    .read_line(&mut request_line)
    .await
    .map_err(|e| RunnerError::Network(format!("reading callback request failed: {e}")))?;
  // A complete request line ends in `\r\n`; no trailing newline means the peer
  // dropped mid-line or blew past the cap.
  if read == 0 || !request_line.ends_with('\n') {
    return Ok(None);
  }
  Ok(Some(
    request_line
      .split_whitespace()
      .nth(1)
      .unwrap_or("")
      .to_owned(),
  ))
}

/// Verify the CSRF state on a `/callback` target and reply. `Ok(Some(code))`
/// on success; the parse error is surfaced after a best-effort `400`.
///
/// Both writes are best-effort: on success the code is already captured, and on
/// rejection the CSRF/parse error terminates the flow regardless of the write.
async fn serve_callback<W: AsyncWriteExt + Unpin>(
  writer: &mut W,
  target: &str,
  state: &str,
) -> Result<Option<String>, RunnerError> {
  match parse_callback_path(target, state) {
    Ok(code) => {
      log_write_err(
        write_http_response(
          writer,
          "HTTP/1.1 200 OK",
          "text/html; charset=utf-8",
          SUCCESS_HTML,
        )
        .await,
      );
      Ok(Some(code))
    },
    Err(e) => {
      log_write_err(
        write_http_response(
          writer,
          "HTTP/1.1 400 Bad Request",
          "text/plain; charset=utf-8",
          "Bad request: the manifest callback was rejected.\n",
        )
        .await,
      );
      Err(e)
    },
  }
}

/// Write one `Connection: close` HTTP/1.1 response with a correct
/// `Content-Length`.
async fn write_http_response<W: AsyncWriteExt + Unpin>(
  writer: &mut W,
  status_line: &str,
  content_type: &str,
  body: &str,
) -> Result<(), RunnerError> {
  let response = format!(
    "{status_line}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
    body.len()
  );
  writer
    .write_all(response.as_bytes())
    .await
    .map_err(|e| RunnerError::Network(format!("writing callback response failed: {e}")))?;
  writer
    .flush()
    .await
    .map_err(|e| RunnerError::Network(format!("flushing callback response failed: {e}")))
}

/// POST the manifest code to `<api_base>/app-manifests/{code}/conversions`,
/// returning the created app's credentials.
///
/// Sets `Accept: application/vnd.github+json` and a `User-Agent` (GitHub's
/// REST API 403s requests without one). On a 2xx the body is parsed by
/// [`parse_conversion`]; a non-2xx is mapped by [`map_conversion_error`].
///
/// # Errors
///
/// Returns `RunnerError::Network` on transport failure or a non-404/422
/// non-2xx status, `RunnerError::Auth` when the code is spent/invalid
/// (404/422), and `RunnerError::Protocol` when a 2xx body does not parse.
pub async fn convert_manifest_code(
  client: &reqwest::Client,
  api_base: &str,
  code: &str,
) -> Result<ConversionResponse, RunnerError> {
  let url = build_conversion_url(api_base, code);
  let response = client
    .post(&url)
    .header("Accept", "application/vnd.github+json")
    .header(
      "User-Agent",
      concat!("toolu-runner/", env!("CARGO_PKG_VERSION")),
    )
    .send()
    .await
    .map_err(|e| {
      RunnerError::Network(format!(
        "app manifest conversion request failed: {e}; re-run `create-app` to start a fresh flow"
      ))
    })?;

  let status = response.status();
  let text = response.text().await.map_err(|e| {
    RunnerError::Network(format!("reading app manifest conversion body failed: {e}"))
  })?;

  if status.is_success() {
    parse_conversion(&text)
  } else {
    Err(map_conversion_error(status.as_u16(), &text))
  }
}

/// Build the `app-manifests/{code}/conversions` endpoint URL under the REST
/// `api_base` (`https://api.github.com` for github.com; a trailing `/` is
/// ignored).
pub fn build_conversion_url(api_base: &str, code: &str) -> String {
  format!(
    "{}/app-manifests/{code}/conversions",
    api_base.trim_end_matches('/')
  )
}

/// Map a non-2xx conversion status into a [`RunnerError`].
///
/// A 404 or 422 means the one-time manifest code was already spent (or was
/// never valid), so the message tells the user to re-run `create-app`. Any
/// other status yields a generic error carrying the status and a short body
/// snippet, and likewise points the user back at `create-app` (re-running
/// always mints a fresh manifest and code, whatever the failure).
pub fn map_conversion_error(status: u16, body: &str) -> RunnerError {
  if status == 404 || status == 422 {
    RunnerError::Auth(format!(
      "GitHub rejected the app manifest code (HTTP {status}) \u{2014} the temporary code is \
       single-use and has been spent or is invalid; re-run `create-app` to generate a fresh \
       manifest and code"
    ))
  } else {
    let snippet: String = body.chars().take(200).collect();
    RunnerError::Network(format!(
      "app manifest conversion failed with HTTP {status}: {snippet}; re-run `create-app` to \
       start a fresh flow"
    ))
  }
}

/// Default wait between claims when the relay's `204` names none (its
/// contract says `Retry-After: 2`).
const RELAY_DEFAULT_RETRY: Duration = Duration::from_secs(2);

/// One claim's answer from the relay.
#[derive(Debug, PartialEq, Eq)]
pub enum RelayClaim {
  /// The code, handed over exactly once.
  Code(String),
  /// Not there yet (or expired, or another secret's): poll again after this.
  Pending(Duration),
}

/// `POST <relay_base>/gh/claim` with `{"secret": secret}` (the get.toolu.sh
/// contract, CodaSignal/toolu.sh#88): `200 {"code"}` once, else `204` with
/// `Retry-After` (seconds, clamped to 1–30, default 2).
///
/// # Errors
///
/// `RunnerError::Network` on transport failure or any non-2xx status but
/// `400` (worth retrying, and the paste fallback stays open);
/// `RunnerError::Protocol` on a `400` (the relay refused the body) or a
/// `200` whose code is not a safe manifest code.
pub async fn claim_relay_code(
  client: &reqwest::Client,
  relay_base: &str,
  secret: &str,
) -> Result<RelayClaim, RunnerError> {
  let url = format!("{}/gh/claim", relay_base.trim_end_matches('/'));
  let response = client
    .post(&url)
    .json(&serde_json::json!({ "secret": secret }))
    .send()
    .await
    .map_err(|e| RunnerError::Network(format!("relay claim request failed: {e}")))?;
  let status = response.status();
  if status == reqwest::StatusCode::NO_CONTENT {
    let wait = response
      .headers()
      .get(reqwest::header::RETRY_AFTER)
      .and_then(|value| value.to_str().ok())
      .and_then(|value| value.trim().parse::<u64>().ok())
      .map_or(RELAY_DEFAULT_RETRY, |secs| {
        Duration::from_secs(secs.clamp(1, 30))
      });
    return Ok(RelayClaim::Pending(wait));
  }
  if status == reqwest::StatusCode::BAD_REQUEST {
    return Err(RunnerError::Protocol(format!(
      "relay claim refused with HTTP {status}"
    )));
  }
  // A relay that is down, not deployed (404) or blocked by a proxy (403)
  // is as good as unreachable: keep polling, the paste stays open.
  if !status.is_success() {
    return Err(RunnerError::Network(format!(
      "relay claim answered HTTP {status}"
    )));
  }
  let body: serde_json::Value = response
    .json()
    .await
    .map_err(|e| RunnerError::Protocol(format!("relay claim body unreadable: {e}")))?;
  match body.get("code").and_then(serde_json::Value::as_str) {
    Some(code) if is_manifest_code(code) => Ok(RelayClaim::Code(code.to_owned())),
    _ => Err(RunnerError::Protocol(
      "relay claim returned no usable manifest code".to_owned(),
    )),
  }
}
