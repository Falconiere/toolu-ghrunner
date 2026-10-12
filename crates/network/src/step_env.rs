//! [`step_env`]: the proxy / CA variables a job exports to its steps.

use shared::NetworkConfig;

use crate::client::{bypass_list, configured_proxy};
use crate::config::{HTTP_PROXY, HTTPS_PROXY, NO_PROXY, first_set};

/// Variables exported to a job's steps, lowest precedence (any workflow,
/// job, `GITHUB_ENV` or step value wins).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StepNetworkEnv {
  /// For host steps: proxy variables in both casings plus
  /// `NODE_EXTRA_CA_CERTS` when a `ca_bundle` is configured.
  pub host: Vec<(String, String)>,
  /// For job / service / Docker-action containers: proxy variables only
  /// (the bundle's host path does not exist inside a container).
  pub container: Vec<(String, String)>,
}

/// The effective settings as step variables.
///
/// Config mode exports the configured proxy as `HTTPS_PROXY` / `https_proxy`
/// / `HTTP_PROXY` / `http_proxy` and the bypass list (with loopback) as
/// `NO_PROXY` / `no_proxy`. Env mode re-exports the runner's own first-set
/// value of each pair in both casings (upstream parity: curl and wget read
/// lower case, Docker upper case). The proxy value keeps its credentials,
/// as upstream's does; callers register [`crate::proxy_secrets`] with the
/// secret masker.
pub fn step_env(cfg: &NetworkConfig) -> StepNetworkEnv {
  let mut proxies = Vec::new();
  let both = |list: &mut Vec<(String, String)>, names: [&str; 2], value: &str| {
    for name in names {
      list.push((name.to_owned(), value.to_owned()));
    }
  };
  if let Some(proxy) = configured_proxy(cfg) {
    both(&mut proxies, HTTPS_PROXY, proxy);
    both(&mut proxies, HTTP_PROXY, proxy);
    both(&mut proxies, NO_PROXY, &bypass_list(cfg).join(","));
  } else {
    for names in [HTTPS_PROXY, HTTP_PROXY, NO_PROXY] {
      if let Some(value) = first_set(names) {
        both(&mut proxies, names, &value);
      }
    }
  }
  let mut host = proxies.clone();
  if let Some(bundle) = &cfg.ca_bundle {
    host.push((
      "NODE_EXTRA_CA_CERTS".to_owned(),
      bundle.display().to_string(),
    ));
  }
  StepNetworkEnv {
    host,
    container: proxies,
  }
}
