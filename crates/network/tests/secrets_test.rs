//! #94 AC-9: proxy credentials registered for masking, and the redacted
//! diagnostic line.

use base64::Engine as _;
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

fn cleared() -> Vec<(&'static str, Option<&'static str>)> {
  PROXY_VARS.iter().map(|k| (*k, None)).collect()
}

fn b64(value: &str) -> String {
  base64::engine::general_purpose::STANDARD.encode(value)
}

#[test]
fn config_proxy_password_raw_decoded_and_basic_credential() {
  let cfg = NetworkConfig {
    proxy: Some("http://alice:p%40ssw0rd@proxy.corp:3128".to_owned()),
    ..NetworkConfig::default()
  };
  let secrets = temp_env::with_vars(cleared(), || network::proxy_secrets(&cfg));
  assert!(secrets.contains(&"p%40ssw0rd".to_owned()), "{secrets:?}");
  assert!(secrets.contains(&"p@ssw0rd".to_owned()), "{secrets:?}");
  assert!(secrets.contains(&b64("alice:p@ssw0rd")), "{secrets:?}");
  assert!(
    !secrets.iter().any(|s| s == "alice"),
    "username is not secret"
  );
}

#[test]
fn short_password_still_yields_a_maskable_basic_credential() {
  let cfg = NetworkConfig {
    proxy: Some("http://al:ab@proxy:3128".to_owned()),
    ..NetworkConfig::default()
  };
  let secrets = temp_env::with_vars(cleared(), || network::proxy_secrets(&cfg));
  let basic = b64("al:ab");
  assert!(basic.len() >= 4);
  assert!(secrets.contains(&basic), "{secrets:?}");
}

#[test]
fn env_mode_proxy_credentials_are_registered_too() {
  let mut vars = cleared();
  vars.retain(|(k, _)| *k != "HTTPS_PROXY");
  vars.push(("HTTPS_PROXY", Some("http://bob:envsecret@proxy:3128")));
  let secrets = temp_env::with_vars(vars, || network::proxy_secrets(&NetworkConfig::default()));
  assert!(secrets.contains(&"envsecret".to_owned()), "{secrets:?}");
}

#[test]
fn no_credentials_no_secrets() {
  let cfg = NetworkConfig {
    proxy: Some("http://proxy:3128".to_owned()),
    ..NetworkConfig::default()
  };
  assert!(temp_env::with_vars(cleared(), || network::proxy_secrets(&cfg)).is_empty());
}

#[test]
fn describe_redacts_and_names_the_mode() -> Result<(), shared::RunnerError> {
  let cfg = NetworkConfig {
    proxy: Some("http://alice:p%40ssw0rd@proxy.corp:3128".to_owned()),
    no_proxy: vec!["a".to_owned(), "b".to_owned()],
    ca_bundle: None,
  };
  let line = temp_env::with_vars(cleared(), || network::describe(&cfg))?;
  assert!(
    !line.contains("p%40ssw0rd") && !line.contains("p@ssw0rd"),
    "{line}"
  );
  assert!(
    line.contains("proxy http://***@proxy.corp:3128 (config)"),
    "{line}"
  );
  assert!(line.contains("no_proxy 2 entries"), "{line}");
  let none = temp_env::with_vars(cleared(), || network::describe(&NetworkConfig::default()))?;
  assert!(none.contains("proxy none"), "{none}");
  Ok(())
}

#[test]
fn lone_userinfo_token_registers_raw_decoded_and_basic_credential() {
  let cfg = NetworkConfig {
    proxy: Some("http://tok%2Fen@proxy:3128".to_owned()),
    ..NetworkConfig::default()
  };
  let secrets = temp_env::with_vars(cleared(), || network::proxy_secrets(&cfg));
  assert!(secrets.contains(&"tok%2Fen".to_owned()), "{secrets:?}");
  assert!(secrets.contains(&"tok/en".to_owned()), "{secrets:?}");
  assert!(secrets.contains(&b64("tok/en:")), "{secrets:?}");
}

#[test]
fn env_mode_reads_all_proxy_too() {
  let mut vars = cleared();
  vars.retain(|(k, _)| *k != "ALL_PROXY");
  vars.push(("ALL_PROXY", Some("http://carol:allsecret@proxy:3128")));
  let secrets = temp_env::with_vars(vars, || network::proxy_secrets(&NetworkConfig::default()));
  assert!(secrets.contains(&"allsecret".to_owned()), "{secrets:?}");
  assert!(secrets.contains(&b64("carol:allsecret")), "{secrets:?}");
}

fn describe_with(set: &[(&'static str, &'static str)]) -> Result<String, shared::RunnerError> {
  let mut vars = cleared();
  vars.retain(|(k, _)| !set.iter().any(|(name, _)| name == k));
  vars.extend(set.iter().map(|(k, v)| (*k, Some(*v))));
  temp_env::with_vars(vars, || network::describe(&NetworkConfig::default()))
}

#[test]
fn describe_env_mode_redacts_the_env_proxy() -> Result<(), shared::RunnerError> {
  assert_eq!(
    describe_with(&[("HTTPS_PROXY", "http://u:pw@p:1")])?,
    "proxy https http://***@p:1 (env HTTPS_PROXY), http direct"
  );
  Ok(())
}

#[test]
fn describe_env_mode_reports_each_scheme_with_all_proxy_fallback() -> Result<(), shared::RunnerError>
{
  let https = ("HTTPS_PROXY", "http://h:s@https-proxy:1");
  let http = ("HTTP_PROXY", "http://h:s@http-proxy:2");
  let all = ("ALL_PROXY", "http://h:s@all-proxy:3");
  assert_eq!(
    describe_with(&[https, http, all])?,
    "proxy https http://***@https-proxy:1 (env HTTPS_PROXY), http http://***@http-proxy:2 (env HTTP_PROXY)"
  );
  assert_eq!(
    describe_with(&[http, all])?,
    "proxy https http://***@all-proxy:3 (env ALL_PROXY), http http://***@http-proxy:2 (env HTTP_PROXY)"
  );
  assert_eq!(
    describe_with(&[all])?,
    "proxy http://***@all-proxy:3 (env ALL_PROXY)"
  );
  assert_eq!(describe_with(&[])?, "proxy none");
  Ok(())
}

#[test]
fn describe_rejects_an_unusable_configured_proxy() {
  let cfg = NetworkConfig {
    proxy: Some("ftp://u:secretpw@x".to_owned()),
    ..NetworkConfig::default()
  };
  let msg = match temp_env::with_vars(cleared(), || network::describe(&cfg)) {
    Err(shared::RunnerError::Config(msg)) => msg,
    other => format!("expected a Config error, got {other:?}"),
  };
  assert_eq!(
    msg,
    "[network] proxy ftp://***@x: scheme must be http or https"
  );
}

#[test]
fn proxy_configured_ignores_blank_values() {
  let set = |value: Option<&str>| NetworkConfig {
    proxy: value.map(str::to_owned),
    ..NetworkConfig::default()
  };
  assert!(network::proxy_configured(&set(Some("http://p:1"))));
  assert!(!network::proxy_configured(&set(Some("   "))));
  assert!(!network::proxy_configured(&set(None)));
}
