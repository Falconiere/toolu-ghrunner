//! `NetworkConfig`'s `Debug` never prints the proxy value (#94 AC-9).

use std::path::PathBuf;

use shared::NetworkConfig;

#[test]
fn debug_of_network_config_shows_only_that_a_proxy_is_set() {
  let cfg = NetworkConfig {
    proxy: Some("http://alice:s3cr3t@proxy.corp:3128".to_owned()),
    no_proxy: vec!["internal.corp".to_owned()],
    ca_bundle: Some(PathBuf::from("/etc/corp/ca.pem")),
  };
  assert_eq!(
    format!("{cfg:?}"),
    r#"NetworkConfig { proxy: Some("<set>"), no_proxy: ["internal.corp"], ca_bundle: Some("/etc/corp/ca.pem") }"#
  );
  // Values `url` cannot parse are hidden the same way.
  let odd = NetworkConfig {
    proxy: Some("user:pa://ss@proxy:3128".to_owned()),
    ..NetworkConfig::default()
  };
  assert!(!format!("{odd:?}").contains("pa://ss"));
  assert_eq!(
    format!("{:?}", NetworkConfig::default()),
    "NetworkConfig { proxy: None, no_proxy: [], ca_bundle: None }"
  );
}
