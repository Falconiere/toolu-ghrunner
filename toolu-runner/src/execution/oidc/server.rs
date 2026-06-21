//! OIDC HTTP server and request handling.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use serde::{Deserialize, Serialize};
use shared::RunnerError;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

use super::claims::{OidcClaims, OidcClaimsParams, OidcConfig, OidcJobContext, OidcMode};
use crate::execution::service_auth::validate_bearer;

/// Shared state for the OIDC Axum server.
struct OidcState {
  config: OidcConfig,
  bearer_token: String,
  job_context: OidcJobContext,
}

/// Local OIDC token server.
///
/// Binds to `127.0.0.1:0` (random port), validates bearer tokens,
/// and serves OIDC tokens via the GitHub-compatible endpoint.
pub struct OidcServer {
  address: SocketAddr,
  shutdown_tx: Option<oneshot::Sender<()>>,
  join_handle: Option<tokio::task::JoinHandle<()>>,
}

impl OidcServer {
  /// Start the OIDC server on a random localhost port.
  ///
  /// # Errors
  ///
  /// Returns `RunnerError::Io` if the TCP listener fails to bind.
  pub async fn start(
    config: OidcConfig,
    bearer_token: String,
    job_context: OidcJobContext,
  ) -> Result<Self, RunnerError> {
    let state = Arc::new(OidcState {
      config,
      bearer_token,
      job_context,
    });

    let app = axum::Router::new()
      .route(
        "/_apis/pipeline/oidc/requestToken",
        post(handle_oidc_request),
      )
      .with_state(state);

    let listener = TcpListener::bind("127.0.0.1:0")
      .await
      .map_err(RunnerError::Io)?;
    let address = listener.local_addr().map_err(RunnerError::Io)?;

    let (shutdown_tx, shutdown_rx) = oneshot::channel();

    let join_handle = tokio::spawn(async move {
      axum::serve(listener, app)
        .with_graceful_shutdown(async {
          let _ = shutdown_rx.await;
        })
        .await
        .ok();
    });

    Ok(Self {
      address,
      shutdown_tx: Some(shutdown_tx),
      join_handle: Some(join_handle),
    })
  }

  /// The socket address the server is listening on.
  pub fn address(&self) -> SocketAddr {
    self.address
  }

  /// The full URL for `ACTIONS_ID_TOKEN_REQUEST_URL`.
  pub fn request_url(&self) -> String {
    format!(
      "http://{}/_apis/pipeline/oidc/requestToken?api-version=1",
      self.address
    )
  }

  /// Gracefully shut down the server.
  pub async fn shutdown(mut self) {
    if let Some(tx) = self.shutdown_tx.take() {
      let _ = tx.send(());
    }
    if let Some(handle) = self.join_handle.take() {
      let _ = handle.await;
    }
  }
}

/// Query parameters for the OIDC token request.
#[derive(Deserialize)]
struct OidcRequestParams {
  #[serde(rename = "api-version")]
  _api_version: Option<String>,
  audience: Option<String>,
}

/// Response body for the OIDC token request.
#[derive(Serialize)]
struct OidcTokenResponse {
  value: String,
}

/// Handle POST /_apis/pipeline/oidc/requestToken
async fn handle_oidc_request(
  State(state): State<Arc<OidcState>>,
  headers: HeaderMap,
  Query(params): Query<OidcRequestParams>,
  Json(_body): Json<serde_json::Value>,
) -> impl IntoResponse {
  if let Err(status) = validate_bearer(&headers, &state.bearer_token) {
    return (status, Json(serde_json::json!({"error": "unauthorized"}))).into_response();
  }

  let audience = params.audience.as_deref();

  match &state.config.mode {
    OidcMode::GitHub { upstream_url } => {
      match proxy_to_github(upstream_url, &headers, audience).await {
        Ok(response) => response.into_response(),
        Err(e) => (
          StatusCode::BAD_GATEWAY,
          Json(serde_json::json!({"error": format!("upstream OIDC error: {e}")})),
        )
          .into_response(),
      }
    },
    OidcMode::Local {
      signing_key,
      issuer_url,
    } => mint_local_token(&state.job_context, issuer_url, signing_key, audience),
  }
}

/// Mint a locally-signed OIDC token for the job context.
fn mint_local_token(
  ctx: &OidcJobContext,
  issuer_url: &str,
  signing_key: &[u8],
  audience: Option<&str>,
) -> axum::response::Response {
  let subject = format!("repo:{}:ref:{}", ctx.repository, ctx.git_ref);
  let claims = OidcClaims::new(&OidcClaimsParams {
    issuer: issuer_url,
    subject: &subject,
    audience,
    repository: &ctx.repository,
    repository_owner: &ctx.repository_owner,
    actor: &ctx.actor,
    event_name: &ctx.event_name,
    git_ref: &ctx.git_ref,
    sha: &ctx.sha,
    workflow: &ctx.workflow,
    run_id: &ctx.run_id,
    run_number: &ctx.run_number,
    run_attempt: &ctx.run_attempt,
  });

  match mint_jwt(&claims, signing_key) {
    Ok(token) => (StatusCode::OK, Json(OidcTokenResponse { value: token })).into_response(),
    Err(e) => (
      StatusCode::INTERNAL_SERVER_ERROR,
      Json(serde_json::json!({"error": format!("JWT signing failed: {e}")})),
    )
      .into_response(),
  }
}

/// Mint a JWT from claims using HMAC-SHA256 signing.
fn mint_jwt(claims: &OidcClaims, signing_key: &[u8]) -> Result<String, RunnerError> {
  let header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256);
  let key = jsonwebtoken::EncodingKey::from_secret(signing_key);
  jsonwebtoken::encode(&header, claims, &key)
    .map_err(|e| RunnerError::Oidc(format!("JWT encode failed: {e}")))
}

/// Build the upstream OIDC requestToken URL, appending `audience` if given.
fn oidc_request_url(upstream_url: &str, audience: Option<&str>) -> String {
  let mut url = format!(
    "{}/_apis/pipeline/oidc/requestToken?api-version=1",
    upstream_url.trim_end_matches('/')
  );
  if let Some(aud) = audience {
    url.push_str(&format!("&audience={aud}"));
  }
  url
}

/// Build the timeout-bounded HTTP client used to proxy OIDC requests.
///
/// A request timeout so a hung upstream can't block the OIDC handler (and the
/// requesting action) forever. Propagates the builder error.
fn oidc_proxy_client() -> Result<reqwest::Client, RunnerError> {
  reqwest::Client::builder()
    .timeout(std::time::Duration::from_secs(60))
    .build()
    .map_err(|e| RunnerError::Oidc(format!("build HTTP client: {e}")))
}

/// Proxy an OIDC token request to GitHub's upstream OIDC provider.
async fn proxy_to_github(
  upstream_url: &str,
  headers: &HeaderMap,
  audience: Option<&str>,
) -> Result<(StatusCode, Json<serde_json::Value>), RunnerError> {
  let client = oidc_proxy_client()?;
  let url = oidc_request_url(upstream_url, audience);

  let mut req = client.post(&url);
  if let Some(auth) = headers.get("Authorization")
    && let Ok(auth_str) = auth.to_str()
  {
    req = req.header("Authorization", auth_str);
  }

  let resp = req
    .json(&serde_json::json!({}))
    .send()
    .await
    .map_err(|e| RunnerError::Oidc(format!("upstream request failed: {e}")))?;

  let status =
    StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
  let body: serde_json::Value = resp
    .json()
    .await
    .map_err(|e| RunnerError::Oidc(format!("upstream response parse failed: {e}")))?;

  Ok((status, Json(body)))
}
