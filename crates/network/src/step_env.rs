//! [`step_env`]: the proxy / CA variables a job exports to its steps.

use shared::NetworkConfig;

use crate::client::{bypass_list, configured_proxy};
use crate::proxy_env::ProxyEnv;

const HTTPS_PROXY: [&str; 2] = ["HTTPS_PROXY", "https_proxy"];
const HTTP_PROXY: [&str; 2] = ["HTTP_PROXY", "http_proxy"];
const NO_PROXY: [&str; 2] = ["NO_PROXY", "no_proxy"];

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
/// `NO_PROXY` / `no_proxy`. Env mode exports what the runner itself uses
/// ([`ProxyEnv::effective`]): per scheme the proxy reqwest goes through (a
/// set-but-empty variable disables its pair, an unparseable one falls back to
/// `ALL_PROXY`), plus the first-set `NO_PROXY` pair, each in both casings
/// (upstream parity: curl and wget read lower case, Docker upper case); with
/// `REQUEST_METHOD` set, nothing. The proxy value keeps its credentials,
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
    let env = ProxyEnv::from_env();
    if !env.cgi {
      if let Some(var) = env.effective(true) {
        both(&mut proxies, HTTPS_PROXY, &var.value);
      }
      if let Some(var) = env.effective(false) {
        both(&mut proxies, HTTP_PROXY, &var.value);
      }
      if let Some(var) = &env.no {
        both(&mut proxies, NO_PROXY, &var.value);
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
