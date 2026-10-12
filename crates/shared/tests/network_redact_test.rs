//! `redact_proxy_url` and `NetworkConfig`'s redacting `Debug` (#94 AC-9).

use std::path::PathBuf;

use shared::NetworkConfig;
use shared::network_redact::redact_proxy_url;

#[test]
fn password_is_replaced_and_user_kept() {
  assert_eq!(
    redact_proxy_url("http://alice:s3cr3t@proxy.corp:3128"),
    "http://alice:***@proxy.corp:3128"
  );
}

#[test]
fn lone_userinfo_is_treated_as_a_token() {
  assert_eq!(
    redact_proxy_url("https://ghp_tokenvalue@proxy.corp/"),
    "https://***@proxy.corp/"
  );
}

#[test]
fn at_sign_in_password_and_path_are_handled() {
  // An unencoded `@` in the password: the LAST `@` ends userinfo.
  assert_eq!(
    redact_proxy_url("http://bob:p@ss@proxy:8080/path"),
    "http://bob:***@proxy:8080/path"
  );
  // Secure by default: an `@` later in the path / query over-redacts rather
  // than risk leaking a password that contained `/` or `?`.
  assert_eq!(
    redact_proxy_url("http://bob:p@ss@proxy:8080/path?q=@x"),
    "http://bob:***@x"
  );
}

#[test]
fn reserved_characters_in_the_password_never_leak() {
  for (raw, expected) in [
    ("http://u:ab#cd@proxy:3128", "http://u:***@proxy:3128"),
    ("http://u:a/b@proxy", "http://u:***@proxy"),
    ("http://u:a?b@proxy:1", "http://u:***@proxy:1"),
    ("http://u:a/b?c#d@proxy:1/p", "http://u:***@proxy:1/p"),
    ("http://tok#en@proxy", "http://***@proxy"),
  ] {
    assert_eq!(redact_proxy_url(raw), expected, "{raw}");
  }
}

#[test]
fn values_without_credentials_or_scheme() {
  assert_eq!(redact_proxy_url("http://proxy:3128"), "http://proxy:3128");
  assert_eq!(
    redact_proxy_url("user:pw@proxy:3128"),
    "user:***@proxy:3128"
  );
  assert_eq!(redact_proxy_url(""), "");
}

#[test]
fn debug_of_network_config_never_prints_the_password() {
  let cfg = NetworkConfig {
    proxy: Some("http://alice:s3cr3t@proxy.corp:3128".to_owned()),
    no_proxy: vec!["internal.corp".to_owned()],
    ca_bundle: Some(PathBuf::from("/etc/corp/ca.pem")),
  };
  let debug = format!("{cfg:?}");
  assert!(!debug.contains("s3cr3t"), "{debug}");
  assert!(debug.contains("alice:***@proxy.corp:3128"), "{debug}");
  assert!(debug.contains("internal.corp") && debug.contains("/etc/corp/ca.pem"));
}
