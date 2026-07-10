//! Selective reverse proxy for the accelerated cache origin.
//!
//! In accelerated mode the runner overrides `ACTIONS_RESULTS_URL` at the local
//! server, but that one origin serves two Twirp services: `CacheService` (we
//! handle locally) and `ArtifactService` (used by `upload-artifact@v4`, which
//! must still reach real GitHub). [`proxied_app`] mounts the cache app's routes
//! locally and forwards everything else — verbatim, `Authorization` intact — to
//! the real upstream.
//!
//! The two failure domains are independent by construction: a connect/transport
//! failure to upstream yields `502` on the proxied (artifact) call only, while
//! the local cache routes keep serving from local NVMe.

use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderMap, HeaderName, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use reqwest::Client;

/// Upper bound on a proxied request or response body buffered in memory.
///
/// The proxied bodies are small Twirp control JSON — the large artifact bytes
/// go straight to Azure blob, not through this origin — so buffering is fine.
const MAX_PROXY_BODY: usize = 64 * 1024 * 1024;

/// Wrap `local` (the cache app router) so that any request NOT matched by a
/// local route is forwarded verbatim to `upstream_base` (the real
/// `ACTIONS_RESULTS_URL`), preserving method, path+query, headers (including
/// `Authorization`), and body, and returning the upstream status/headers/body.
///
/// The forward is installed as [`axum::Router::fallback`], which fires only when
/// no local route matches — so cache / blob / v1 routes stay local and
/// everything else forwards. A connect/transport failure to upstream yields
/// `502` (never takes the local cache down).
pub fn proxied_app(local: axum::Router, upstream_base: String, client: Client) -> axum::Router {
  local.fallback(move |req: Request| {
    let upstream_base = upstream_base.clone();
    let client = client.clone();
    async move { forward(req, &upstream_base, &client).await }
  })
}

/// Forward `req` to `{upstream_base}{path+query}` and translate the reply back.
///
/// A failure to send the upstream request — or to read either body — becomes a
/// `502`, so an unreachable upstream never fails the local cache routes.
async fn forward(req: Request, upstream_base: &str, client: &Client) -> Response {
  let (parts, body) = req.into_parts();
  let bytes = match axum::body::to_bytes(body, MAX_PROXY_BODY).await {
    Ok(bytes) => bytes,
    Err(e) => return bad_gateway(&format!("read request body: {e}")),
  };
  let url = build_upstream_url(upstream_base, &parts.uri);
  let headers = copy_request_headers(&parts.headers);
  let sent = client
    .request(parts.method, url)
    .headers(headers)
    .body(bytes)
    .send()
    .await;
  match sent {
    Ok(resp) => translate_response(resp).await,
    Err(e) => bad_gateway(&format!("upstream request failed: {e}")),
  }
}

/// Join `upstream_base` (trailing `/` trimmed) with the request path and query.
fn build_upstream_url(upstream_base: &str, uri: &Uri) -> String {
  let base = upstream_base.trim_end_matches('/');
  let tail = uri
    .path_and_query()
    .map_or_else(|| uri.path().to_owned(), |pq| pq.as_str().to_owned());
  format!("{base}{tail}")
}

/// Copy every request header except hop-by-hop headers, so `Authorization` and
/// `Content-Type` reach upstream while `host` / `content-length` / `connection`
/// are left for `reqwest` to set from the new request.
///
/// `accept-encoding` is also stripped: this proxy passes the upstream body
/// through verbatim and copies only `Content-Type`, so it must not let upstream
/// compress a response it would then relay without a `Content-Encoding` header.
fn copy_request_headers(src: &HeaderMap) -> HeaderMap {
  let mut out = HeaderMap::with_capacity(src.len());
  for (name, value) in src {
    if is_hop_by_hop(name) || name == header::ACCEPT_ENCODING {
      continue;
    }
    out.append(name.clone(), value.clone());
  }
  out
}

/// Whether `name` is a hop-by-hop header that must not be forwarded verbatim.
fn is_hop_by_hop(name: &HeaderName) -> bool {
  matches!(
    name.as_str(),
    "host"
      | "content-length"
      | "connection"
      | "keep-alive"
      | "proxy-authenticate"
      | "proxy-authorization"
      | "te"
      | "trailer"
      | "transfer-encoding"
      | "upgrade"
  )
}

/// Translate an upstream `reqwest` reply into an axum response: status,
/// `Content-Type`, and the buffered body bytes.
async fn translate_response(resp: reqwest::Response) -> Response {
  let status = resp.status();
  let content_type = resp.headers().get(header::CONTENT_TYPE).cloned();
  let bytes = match resp.bytes().await {
    Ok(bytes) => bytes,
    Err(e) => return bad_gateway(&format!("read upstream body: {e}")),
  };
  let mut builder = Response::builder().status(status);
  if let Some(ct) = content_type {
    builder = builder.header(header::CONTENT_TYPE, ct);
  }
  match builder.body(Body::from(bytes)) {
    Ok(resp) => resp,
    Err(e) => bad_gateway(&format!("build proxied response: {e}")),
  }
}

/// A `502 Bad Gateway` with a short text body describing the upstream failure.
fn bad_gateway(msg: &str) -> Response {
  (StatusCode::BAD_GATEWAY, msg.to_owned()).into_response()
}
