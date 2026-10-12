//! `[network]` config section (#94): explicit proxy, bypass list and extra CA.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use shared::NetworkConfig;

/// `[network]` in `config.toml`. Every key is optional and an absent section
/// is env mode (today's behaviour); `save_config` omits the table when empty.
///
/// ```toml
/// [network]
/// proxy     = "http://user:pass@proxy.corp:3128"
/// no_proxy  = ["internal.corp", ".svc.local", "10.0.0.0/8"]
/// ca_bundle = "~/certs/corp-root.pem"
/// ```
#[derive(Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct NetworkSection {
  /// `http://` / `https://` proxy for http and https destinations; may carry
  /// `user:password@` (never printed — see the redacting `Debug`).
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub proxy: Option<String>,
  /// Bypass entries in `NO_PROXY` syntax (only used with `proxy`).
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub no_proxy: Vec<String>,
  /// PEM bundle of extra trusted CA certificates; `~` expands to the home
  /// directory and the result must be absolute.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub ca_bundle: Option<String>,
}

impl std::fmt::Debug for NetworkSection {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    std::fmt::Debug::fmt(&self.resolve(), f)
  }
}

impl NetworkSection {
  /// Whether nothing is configured (the table is then omitted on save).
  pub fn is_empty(&self) -> bool {
    self.proxy.is_none() && self.no_proxy.is_empty() && self.ca_bundle.is_none()
  }

  /// The runtime [`NetworkConfig`]: blank `no_proxy` entries dropped and the
  /// rest trimmed, `ca_bundle` tilde-expanded. Validation (absolute path,
  /// readable PEM, proxy URL shape) happens where clients are built, so a
  /// bad value fails closed at `run` / `check` start with a clear error.
  pub fn resolve(&self) -> NetworkConfig {
    NetworkConfig {
      proxy: self.proxy.clone(),
      no_proxy: self
        .no_proxy
        .iter()
        .map(|entry| entry.trim().to_owned())
        .filter(|entry| !entry.is_empty())
        .collect(),
      ca_bundle: self
        .ca_bundle
        .as_deref()
        .map(|raw| shared::paths::expand_tilde(&PathBuf::from(raw.trim()))),
    }
  }
}
