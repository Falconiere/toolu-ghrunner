//! Forwarder: extract real GitHub service URLs + runtime token from the job
//! message and emit them as the `ACTIONS_*` env vars the JS toolkit `@v4`
//! actions read, so they talk to real GitHub (the official runner's design).
//!
//! Forwarder mode is the default; [`crate::execution::job_runner`] injects
//! [`forward_env`] per step type (Node, container, `run:`). Offline mode
//! starts only the local v1 cache instead (see `[services] mode`).

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
  /// `actions_uses_cache_service_v2` job variable parsed as boolean true;
  /// exported as `ACTIONS_CACHE_SERVICE_V2=True` to Node actions only.
  pub cache_service_v2: bool,
  /// `actions_cache_mode` job variable, exported unchanged as
  /// `ACTIONS_CACHE_MODE` when non-empty (independent of the v2 flag).
  pub cache_mode: Option<String>,
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
/// `actions_uses_cache_service_v2` / `actions_cache_mode` job variables.
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

  // Upstream `Variables` matches names case-insensitively.
  let variable = |name: &str| {
    msg
      .variables
      .iter()
      .find(|(key, _)| key.eq_ignore_ascii_case(name))
      .map(|(_, v)| v.value.as_str())
  };
  let cache_service_v2 = variable("actions_uses_cache_service_v2").is_some_and(is_true);
  let cache_mode = variable("actions_cache_mode")
    .filter(|v| !v.is_empty())
    .map(str::to_owned);

  ServiceUrls {
    results_url: data_value("ResultsServiceUrl"),
    runtime_url: data_value("PipelinesServiceUrl"),
    cache_url: data_value("CacheServerUrl"),
    cache_service_v2,
    cache_mode,
    id_token_request_url: data_value("GenerateIdTokenUrl"),
    runtime_token,
  }
}

/// The `ACTIONS_*` pairs each step type receives (official v2.338.2 mapping).
///
/// Held on the execution context, never on the job-wide env, so a `run:` step
/// cannot read the runtime token.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServiceEnv {
  /// Node actions: runtime, results, cache and OIDC vars, v2 flag, cache mode.
  pub node: Vec<(String, String)>,
  /// Container actions: the Node set without the v2 flag.
  pub container: Vec<(String, String)>,
  /// `run:` steps: only the OIDC request URL and token.
  pub script: Vec<(String, String)>,
}

impl ServiceEnv {
  /// Set `key` in both action sets (replacing an existing value).
  pub fn set_action(&mut self, key: &str, value: &str) {
    for set in [&mut self.node, &mut self.container] {
      set.retain(|(k, _)| k != key);
      set.push((key.to_owned(), value.to_owned()));
    }
  }

  /// Set `key` in the Node set only (replacing an existing value).
  pub fn set_node(&mut self, key: &str, value: &str) {
    self.node.retain(|(k, _)| k != key);
    self.node.push((key.to_owned(), value.to_owned()));
  }

  /// Every `*_TOKEN` value, for masking.
  pub fn tokens(&self) -> impl Iterator<Item = &str> {
    self
      .node
      .iter()
      .chain(&self.container)
      .chain(&self.script)
      .filter(|(k, _)| k.ends_with("_TOKEN"))
      .map(|(_, v)| v.as_str())
  }
}

/// Build the per-step-type `ACTIONS_*` env.
///
/// `None` URL fields are OMITTED (never emitted as empty, matching the
/// official runner). `ACTIONS_ID_TOKEN_REQUEST_TOKEN` is emitted only with its
/// URL. A missing service URL logs a `WARN` and is skipped — the dependent
/// action fails itself with GitHub's own error rather than failing the job.
pub fn forward_env(u: &ServiceUrls) -> ServiceEnv {
  // Without the runtime token, every forwarded service URL is uncredentialed:
  // emitting the `ACTIONS_*_URL` vars but not the token leaves actions in a
  // URLs-without-creds half-state that fails with an opaque 401/403. Omit the
  // whole forwarder set and WARN so the failure is clear and early.
  if u.runtime_token.is_empty() {
    tracing::warn!(
      "forwarder: no runtime token in job message; omitting all ACTIONS_* service vars \
       (URLs + token) so toolkit actions fail fast rather than unauthenticated"
    );
    return ServiceEnv::default();
  }

  let mut action: Vec<(String, String)> = Vec::new();
  push_url(&mut action, "ACTIONS_RESULTS_URL", u.results_url.as_deref());
  push_url(&mut action, "ACTIONS_RUNTIME_URL", u.runtime_url.as_deref());
  push_url(&mut action, "ACTIONS_CACHE_URL", u.cache_url.as_deref());
  action.push(("ACTIONS_RUNTIME_TOKEN".to_owned(), u.runtime_token.clone()));

  let mut oidc: Vec<(String, String)> = Vec::new();
  if let Some(url) = u.id_token_request_url.as_deref() {
    oidc.push(("ACTIONS_ID_TOKEN_REQUEST_URL".to_owned(), url.to_owned()));
    oidc.push((
      "ACTIONS_ID_TOKEN_REQUEST_TOKEN".to_owned(),
      u.runtime_token.clone(),
    ));
  } else {
    tracing::warn!("forwarder: ACTIONS_ID_TOKEN_REQUEST_URL absent from job message; omitting");
  }
  action.extend(oidc.clone());
  if let Some(mode) = &u.cache_mode {
    action.push(("ACTIONS_CACHE_MODE".to_owned(), mode.clone()));
  }

  let mut node = action.clone();
  if u.cache_service_v2 {
    node.push(("ACTIONS_CACHE_SERVICE_V2".to_owned(), "True".to_owned()));
  }
  ServiceEnv {
    node,
    container: action,
    script: oidc,
  }
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

/// Upstream `bool.TryParse`: only `true` (any case, trimmed) is on.
fn is_true(value: &str) -> bool {
  value.trim().eq_ignore_ascii_case("true")
}
