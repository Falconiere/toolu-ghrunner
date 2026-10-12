//! #94 AC-5 (unit level): the values `network::step_env` exports to host
//! steps and containers in config mode and env mode.

use std::collections::HashMap;
use std::path::PathBuf;

use shared::NetworkConfig;

const PROXY_VARS: [&str; 9] = [
  "HTTPS_PROXY",
  "https_proxy",
  "HTTP_PROXY",
  "http_proxy",
  "ALL_PROXY",
  "all_proxy",
  "NO_PROXY",
  "no_proxy",
  "REQUEST_METHOD",
];

fn with_env<T>(set: &[(&'static str, &'static str)], f: impl FnOnce() -> T) -> T {
  let vars: Vec<(&str, Option<&str>)> = PROXY_VARS
    .iter()
    .map(|name| (*name, set.iter().find(|(k, _)| k == name).map(|(_, v)| *v)))
    .collect();
  temp_env::with_vars(vars, f)
}

fn map(pairs: Vec<(String, String)>) -> HashMap<String, String> {
  pairs.into_iter().collect()
}

#[test]
fn config_mode_exports_both_casings_loopback_and_node_ca() {
  let cfg = NetworkConfig {
    proxy: Some("http://u:pw12@proxy.corp:3128".to_owned()),
    no_proxy: vec!["internal.corp".to_owned(), ".svc".to_owned()],
    ca_bundle: Some(PathBuf::from("/etc/corp/ca.pem")),
  };
  let exported = with_env(&[("HTTPS_PROXY", "http://ambient:1")], || network::step_env(&cfg));
  let host = map(exported.host);
  for key in ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"] {
    assert_eq!(host.get(key).map(String::as_str), Some("http://u:pw12@proxy.corp:3128"), "{key}");
  }
  let bypass = "internal.corp,.svc,localhost,127.0.0.1,::1";
  assert_eq!(host.get("NO_PROXY").map(String::as_str), Some(bypass));
  assert_eq!(host.get("no_proxy").map(String::as_str), Some(bypass));
  assert_eq!(host.get("NODE_EXTRA_CA_CERTS").map(String::as_str), Some("/etc/corp/ca.pem"));
  let container = map(exported.container);
  assert_eq!(container.len(), 6, "{container:?}");
  assert!(!container.contains_key("NODE_EXTRA_CA_CERTS"));
  assert_eq!(container.get("https_proxy"), host.get("https_proxy"));
}

#[test]
fn env_mode_reexports_the_first_set_value_in_both_casings() {
  let exported = with_env(
    &[
      ("HTTPS_PROXY", "http://upper:1"),
      ("https_proxy", "http://lower:2"),
      ("no_proxy", "corp"),
    ],
    || network::step_env(&NetworkConfig::default()),
  );
  let host = map(exported.host);
  assert_eq!(host.get("HTTPS_PROXY").map(String::as_str), Some("http://upper:1"));
  assert_eq!(host.get("https_proxy").map(String::as_str), Some("http://upper:1"));
  assert_eq!(host.get("NO_PROXY").map(String::as_str), Some("corp"));
  assert_eq!(host.get("no_proxy").map(String::as_str), Some("corp"));
  assert!(!host.contains_key("HTTP_PROXY") && !host.contains_key("http_proxy"));
  assert!(!host.contains_key("NODE_EXTRA_CA_CERTS"));
}

#[test]
fn nothing_configured_exports_nothing() {
  let exported = with_env(&[], || network::step_env(&NetworkConfig::default()));
  assert!(exported.host.is_empty() && exported.container.is_empty());
}

#[test]
fn bundle_alone_exports_only_node_extra_ca_certs() {
  let cfg = NetworkConfig {
    ca_bundle: Some(PathBuf::from("/opt/ca.pem")),
    ..NetworkConfig::default()
  };
  let exported = with_env(&[], || network::step_env(&cfg));
  assert_eq!(
    exported.host,
    vec![("NODE_EXTRA_CA_CERTS".to_owned(), "/opt/ca.pem".to_owned())]
  );
  assert!(exported.container.is_empty());
}
