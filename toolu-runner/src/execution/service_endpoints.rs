//! Forwarder: extract real GitHub service URLs + runtime token from the job
//! message and emit them as the `ACTIONS_*` env vars the JS toolkit `@v4`
//! actions read, so they talk to real GitHub (the official runner's design).
//!
//! Forwarder mode is the default; [`crate::execution::job_runner`] injects
//! [`forward_env`] into the per-job env that every step inherits. Offline mode
//! hosts local services instead (see `[services] mode`).

use shared::AgentJobRequestMessage;

/// Name of the `SystemVssConnection` endpoint that carries the service URLs.
const SYSTEM_CONNECTION: &str = "SystemVssConnection";

/// Real GitHub service URLs + runtime token extracted from the job message.
///
/// URL fields are `Option` because a given message may omit a service the
/// workflow never uses; the token is always present (the OAuth `AccessToken`).
/// A `None` URL is OMITTED from [`forward_env`] (never emitted as empty).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceUrls {
  /// `ACTIONS_RESULTS_URL` — results/artifacts receiver (`@v4` upload).
  pub results_url: Option<String>,
  /// `ACTIONS_RUNTIME_URL` — pipelines service (legacy runtime/artifacts).
  pub runtime_url: Option<String>,
  /// `ACTIONS_CACHE_URL` — cache service (`actions/cache@v4`).
  pub cache_url: Option<String>,
  /// `ACTIONS_CACHE_SERVICE_V2` — emitted only when `true`.
  pub cache_service_v2: bool,
  /// `ACTIONS_ID_TOKEN_REQUEST_URL` — OIDC token endpoint.
  pub id_token_request_url: Option<String>,
  /// Forwarded runtime token (the OAuth `AccessToken`), reused for the
  /// id-token request token.
  pub runtime_token: String,
}

/// Extract the service URLs + runtime token from the `SystemVssConnection`
/// endpoint (`data` map + `authorization`) and the job `variables`.
///
/// Keys (from the committed fixture + documented shape): `ResultsServiceUrl`,
/// `PipelinesServiceUrl`→runtime, `CacheServerUrl`, `GenerateIdTokenUrl`,
/// `authorization.parameters["AccessToken"]`→token, and the
/// `ACTIONS_CACHE_SERVICE_V2` variable.
///
/// TODO: confirm these exact key spellings and where the cache-v2 flag lives
/// (job `Variables` vs endpoint `data`) against a captured live job message.
pub fn extract_service_urls(msg: &AgentJobRequestMessage) -> ServiceUrls {
  let endpoint = msg
    .resources
    .endpoints
    .iter()
    .find(|e| e.name == SYSTEM_CONNECTION);

  let data_value = |key: &str| -> Option<String> {
    endpoint
      .and_then(|e| e.data.get(key))
      .filter(|v| !v.is_empty())
      .cloned()
  };

  let runtime_token = endpoint
    .and_then(|e| e.authorization.as_ref())
    .and_then(|a| a.parameters.get("AccessToken"))
    .filter(|v| !v.is_empty())
    .cloned()
    .unwrap_or_default();

  let cache_service_v2 = msg
    .variables
    .get("ACTIONS_CACHE_SERVICE_V2")
    .map(|v| is_truthy(&v.value))
    .unwrap_or(false);

  ServiceUrls {
    results_url: data_value("ResultsServiceUrl"),
    runtime_url: data_value("PipelinesServiceUrl"),
    cache_url: data_value("CacheServerUrl"),
    cache_service_v2,
    id_token_request_url: data_value("GenerateIdTokenUrl"),
    runtime_token,
  }
}

/// Build the `ACTIONS_*` env pairs to inject into every step's environment.
///
/// `None` URL fields are OMITTED (never emitted as empty, matching the
/// official runner); `ACTIONS_CACHE_SERVICE_V2` is emitted only when `true`.
/// The runtime token is reused for `ACTIONS_ID_TOKEN_REQUEST_TOKEN`. A missing
/// service URL logs a `WARN` and is skipped — the dependent action fails itself
/// with GitHub's own error rather than failing the whole job.
pub fn forward_env(u: &ServiceUrls) -> Vec<(String, String)> {
  // Without the runtime token, every forwarded service URL is uncredentialed:
  // emitting the `ACTIONS_*_URL` vars but not the token leaves actions in a
  // URLs-without-creds half-state that fails with an opaque 401/403. Omit the
  // whole forwarder set and WARN so the failure is clear and early.
  if u.runtime_token.is_empty() {
    tracing::warn!(
      "forwarder: no runtime token in job message; omitting all ACTIONS_* service vars \
       (URLs + token) so toolkit actions fail fast rather than unauthenticated"
    );
    return Vec::new();
  }

  let mut out: Vec<(String, String)> = Vec::new();

  push_url(&mut out, "ACTIONS_RESULTS_URL", u.results_url.as_deref());
  push_url(&mut out, "ACTIONS_RUNTIME_URL", u.runtime_url.as_deref());
  push_url(&mut out, "ACTIONS_CACHE_URL", u.cache_url.as_deref());
  push_url(
    &mut out,
    "ACTIONS_ID_TOKEN_REQUEST_URL",
    u.id_token_request_url.as_deref(),
  );

  out.push(("ACTIONS_RUNTIME_TOKEN".to_owned(), u.runtime_token.clone()));
  out.push((
    "ACTIONS_ID_TOKEN_REQUEST_TOKEN".to_owned(),
    u.runtime_token.clone(),
  ));

  if u.cache_service_v2 {
    out.push(("ACTIONS_CACHE_SERVICE_V2".to_owned(), "true".to_owned()));
  }

  out
}

/// Push `var=value` if `value` is present, else `WARN` + omit.
fn push_url(out: &mut Vec<(String, String)>, var: &str, value: Option<&str>) {
  if let Some(v) = value {
    out.push((var.to_owned(), v.to_owned()));
  } else {
    tracing::warn!(
      var,
      "forwarder: service URL absent from job message; omitting"
    );
  }
}

/// Truthy test for the cache-v2 feature flag value (`"true"`/`"1"`).
fn is_truthy(value: &str) -> bool {
  matches!(value.trim().to_ascii_lowercase().as_str(), "true" | "1")
}
