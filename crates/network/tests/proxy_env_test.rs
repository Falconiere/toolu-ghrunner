//! `network::proxy_env`: the one proxy-env reading, URL redaction and
//! credential extraction (moved from the bin's `check_env`, #93), plus the
//! #94 regressions — env mode matches what reqwest really does (B1), masking
//! fails safe (B2), redaction never echoes a mis-parsed value (B3).

use crate::{proxy_fixtures, tls_fixtures};

use network::proxy_env::UNPARSEABLE;
use network::{ProxyEnv, ProxyVar, redact_url, url_secrets};
use proxy_fixtures::RecordingProxy;
use shared::{NetworkConfig, SecretMasker};
use tls_fixtures::{FixtureResult, Leaf, Pki, TlsTarget};

/// Every variable the system proxy reads, cleared unless a test sets it.
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

/// `PROXY_VARS` all unset except `set`.
fn env_with<'a>(set: &[(&'a str, &'a str)]) -> Vec<(&'a str, Option<&'a str>)> {
  PROXY_VARS
    .iter()
    .map(|name| {
      let value = set.iter().find(|(key, _)| key == name).map(|(_, v)| *v);
      (*name, value)
    })
    .collect()
}

fn var(name: &'static str, value: &str) -> ProxyVar {
  ProxyVar {
    name,
    value: value.to_owned(),
  }
}

#[test]
fn redact_url_hides_userinfo_and_drops_path_and_query() {
  assert_eq!(
    redact_url("http://alice:s3cr3t-pw@proxy.internal:3128/path?token=abc"),
    "http://***@proxy.internal:3128"
  );
  assert_eq!(
    redact_url("http://alice@proxy.internal:3128"),
    "http://***@proxy.internal:3128"
  );
}

#[test]
fn redact_url_keeps_a_credential_free_url_and_socket_paths() {
  assert_eq!(
    redact_url("http://proxy.internal:3128/"),
    "http://proxy.internal:3128"
  );
  assert_eq!(
    redact_url("unix:///var/run/docker.sock"),
    "unix:///var/run/docker.sock"
  );
  assert_eq!(
    redact_url("tcp://bob:hunter22@10.0.0.5:2376"),
    "tcp://***@10.0.0.5:2376"
  );
}

#[test]
fn redact_url_treats_a_schemeless_proxy_as_http_without_leaking() {
  // hyper-util defaults a scheme-less proxy value to http; the `url` crate
  // would otherwise parse `alice` as the scheme and print the password.
  assert_eq!(
    redact_url("alice:s3cr3t-pw@proxy.internal:3128"),
    "http://***@proxy.internal:3128"
  );
}

#[test]
fn redact_url_never_echoes_an_unparseable_or_empty_value() {
  let shown = redact_url("http://[not a host:s3cr3t-pw");
  assert_eq!(shown, UNPARSEABLE);
  assert_eq!(redact_url(""), "(set, empty)");
}

/// B3: a `://` later in the value is not a scheme separator, and an `@`
/// outside the parsed userinfo makes the value unparseable.
#[test]
fn redact_url_only_takes_a_leading_scheme_and_never_echoes_a_misparse() {
  assert_eq!(
    redact_url("user:pw123@host:3128/x?y=http://z"),
    "http://***@host:3128"
  );
  assert_eq!(redact_url("user:pa://ss@proxy:3128"), UNPARSEABLE);
  for raw in [
    "http://u:pwd9z/b@proxy",
    "http://u:123#pwd9z@proxy",
    "http://u:p@pwd9z/x@proxy:3128",
    "http://u:1?pwd9z@proxy",
  ] {
    assert_eq!(redact_url(raw), UNPARSEABLE, "{raw}");
  }
}

#[test]
fn url_secrets_lists_every_credential_spelling() {
  let secrets = url_secrets("http://alice:s3cr3t-pw@proxy.internal:3128");
  for expected in [
    "alice",
    "s3cr3t-pw",
    "alice:s3cr3t-pw",
    "YWxpY2U6czNjcjN0LXB3",
  ] {
    assert!(
      secrets.iter().any(|s| s == expected),
      "{expected} missing: {secrets:?}"
    );
  }
  assert!(url_secrets("http://proxy.internal:3128").is_empty());
  assert_eq!(
    url_secrets("http://[bad:s3cr3t-pw"),
    vec!["http://[bad:s3cr3t-pw"]
  );
}

#[test]
fn url_secrets_percent_decodes_like_the_proxy_auth_header() {
  let secrets = url_secrets("http://alice:p%40ss%3Aword@proxy.internal:3128");
  assert!(secrets.iter().any(|s| s == "p@ss:word"), "{secrets:?}");
  assert!(secrets.iter().any(|s| s == "p%40ss%3Aword"), "{secrets:?}");
}

/// B2: values reqwest accepts but a strict parser rejects, and values whose
/// password holds an unencoded `/ ? #`, are still masked — whole if need be.
#[test]
fn url_secrets_fail_safe_for_schemeless_and_misparsed_values() {
  let schemeless = url_secrets("alice:s3cretpw@proxy.corp:3128");
  assert!(schemeless.iter().any(|s| s == "s3cretpw"), "{schemeless:?}");
  for raw in [
    "http://u:pa/ss9z@proxy",
    "http://u:123#pwd9z@proxy",
    "user:pa://ss@proxy:3128",
  ] {
    assert_eq!(url_secrets(raw), vec![raw.to_owned()], "{raw}");
  }
}

#[test]
fn effective_proxy_prefers_the_scheme_variable_then_all_proxy() {
  let env = ProxyEnv {
    http: Some(var("HTTP_PROXY", "http://h.internal:1")),
    all: Some(var("all_proxy", "http://a.internal:2")),
    ..ProxyEnv::default()
  };
  assert_eq!(env.effective(true).map(|v| v.name), Some("all_proxy"));
  assert_eq!(env.effective(false).map(|v| v.name), Some("HTTP_PROXY"));
}

#[test]
fn unparseable_scheme_variable_falls_back_to_all_proxy_like_hyper_util() {
  let env = ProxyEnv {
    https: Some(var("HTTPS_PROXY", "")),
    all: Some(var("ALL_PROXY", "http://a.internal:2")),
    ..ProxyEnv::default()
  };
  assert_eq!(env.effective(true).map(|v| v.name), Some("ALL_PROXY"));
}

#[test]
fn cgi_mode_disables_every_proxy() {
  let env = ProxyEnv {
    https: Some(var("HTTPS_PROXY", "http://h.internal:1")),
    http: Some(var("HTTP_PROXY", "http://h.internal:1")),
    cgi: true,
    ..ProxyEnv::default()
  };
  assert!(env.effective(true).is_none());
  assert!(env.effective(false).is_none());
}

#[test]
fn proxy_var_debug_is_redacted() {
  let shown = format!("{:?}", var("HTTPS_PROXY", "http://u:s3cr3t-pw@p:1"));
  assert!(!shown.contains("s3cr3t-pw"), "{shown}");
}

/// A client built under `env`, with the generated CA trusted, plus the
/// `describe` line and host step export computed under the same env.
/// What the runner derives from one proxy environment.
struct EnvView {
  /// A client built under it (reqwest reads the variables at build time).
  client: reqwest::Client,
  /// The `describe` line.
  line: String,
  /// The host step export.
  vars: Vec<(String, String)>,
  /// What the masker learns.
  secrets: Vec<String>,
}

fn env_mode_view(env: &[(&str, &str)], cfg: &NetworkConfig) -> FixtureResult<EnvView> {
  temp_env::with_vars(env_with(env), || -> FixtureResult<EnvView> {
    Ok(EnvView {
      client: network::client_builder(cfg)?.build()?,
      line: network::describe(cfg)?,
      vars: network::step_env(cfg).host,
      secrets: network::proxy_secrets(cfg),
    })
  })
}

fn exported<'a>(vars: &'a [(String, String)], name: &str) -> Option<&'a str> {
  vars
    .iter()
    .find(|(key, _)| key == name)
    .map(|(_, value)| value.as_str())
}

/// B1: `HTTPS_PROXY=` (set, empty) disables the pair even with a live
/// `https_proxy` — reqwest goes direct, and so do `describe` and the step
/// export (no dead proxy handed to steps).
#[tokio::test]
async fn set_but_empty_upper_case_disables_the_pair_everywhere() -> FixtureResult<()> {
  let pki = Pki::new()?;
  let dir = tempfile::tempdir()?;
  let target = TlsTarget::serve(
    pki.server_config(&["127.0.0.1"], Leaf::Valid)?,
    b"ok".to_vec(),
  )
  .await?;
  let proxy = RecordingProxy::start(None).await?;
  let proxy_url = proxy.url(None);
  let cfg = NetworkConfig {
    ca_bundle: Some(pki.write_bundle(dir.path())?),
    ..NetworkConfig::default()
  };
  let EnvView {
    client, line, vars, ..
  } = env_mode_view(
    &[("HTTPS_PROXY", ""), ("https_proxy", proxy_url.as_str())],
    &cfg,
  )?;
  let status = client
    .get(format!("https://127.0.0.1:{}/", target.port()))
    .send()
    .await?
    .status();
  assert_eq!(status, 200);
  assert!(proxy.hits().is_empty(), "{:?}", proxy.hits());
  assert_eq!(target.requests().len(), 1);
  assert!(line.starts_with("proxy none,"), "{line}");
  assert_eq!(exported(&vars, "HTTPS_PROXY"), None, "{vars:?}");
  assert_eq!(exported(&vars, "https_proxy"), None, "{vars:?}");
  Ok(())
}

/// B1: `REQUEST_METHOD` (CGI guard) — reqwest ignores the proxy variables,
/// so `describe` says so and nothing is exported.
#[tokio::test]
async fn request_method_disables_the_env_proxy_everywhere() -> FixtureResult<()> {
  let pki = Pki::new()?;
  let dir = tempfile::tempdir()?;
  let target = TlsTarget::serve(
    pki.server_config(&["127.0.0.1"], Leaf::Valid)?,
    b"ok".to_vec(),
  )
  .await?;
  let proxy = RecordingProxy::start(None).await?;
  let proxy_url = proxy.url(None);
  let cfg = NetworkConfig {
    ca_bundle: Some(pki.write_bundle(dir.path())?),
    ..NetworkConfig::default()
  };
  let EnvView {
    client, line, vars, ..
  } = env_mode_view(
    &[
      ("REQUEST_METHOD", "GET"),
      ("HTTPS_PROXY", proxy_url.as_str()),
    ],
    &cfg,
  )?;
  let status = client
    .get(format!("https://127.0.0.1:{}/", target.port()))
    .send()
    .await?
    .status();
  assert_eq!(status, 200);
  assert!(proxy.hits().is_empty(), "{:?}", proxy.hits());
  assert!(
    line.starts_with("proxy none (env: REQUEST_METHOD is set"),
    "{line}"
  );
  assert!(
    vars.iter().all(|(key, _)| key == "NODE_EXTRA_CA_CERTS"),
    "{vars:?}"
  );
  Ok(())
}

/// B1: an unparseable `HTTPS_PROXY` falls back to `ALL_PROXY` — reqwest
/// tunnels through it, and the export names that same proxy.
#[tokio::test]
async fn unparseable_scheme_variable_falls_back_to_all_proxy_everywhere() -> FixtureResult<()> {
  let pki = Pki::new()?;
  let dir = tempfile::tempdir()?;
  let target = TlsTarget::serve(
    pki.server_config(&["target.test"], Leaf::Valid)?,
    b"ok".to_vec(),
  )
  .await?;
  let proxy = RecordingProxy::start(None).await?;
  let proxy_url = proxy.url(None);
  let cfg = NetworkConfig {
    ca_bundle: Some(pki.write_bundle(dir.path())?),
    ..NetworkConfig::default()
  };
  let EnvView {
    client, line, vars, ..
  } = env_mode_view(
    &[
      ("HTTPS_PROXY", "http://[not a host"),
      ("ALL_PROXY", proxy_url.as_str()),
    ],
    &cfg,
  )?;
  let status = client
    .get(format!("https://target.test:{}/", target.port()))
    .send()
    .await?
    .status();
  assert_eq!(status, 200);
  let lines: Vec<String> = proxy.hits().into_iter().map(|hit| hit.line).collect();
  assert_eq!(
    lines,
    vec![format!("CONNECT target.test:{} HTTP/1.1", target.port())]
  );
  assert!(
    line.starts_with(&format!("proxy {proxy_url} (env ALL_PROXY)")),
    "{line}"
  );
  assert_eq!(exported(&vars, "HTTPS_PROXY"), Some(proxy_url.as_str()));
  assert_eq!(exported(&vars, "https_proxy"), Some(proxy_url.as_str()));
  Ok(())
}

/// B2: an env-mode value reqwest accepts without a scheme is exported to
/// steps verbatim — and the job masker hides its password.
#[test]
fn schemeless_env_proxy_is_exported_and_masked() -> FixtureResult<()> {
  let raw = "alice:s3cretpw@proxy.corp:3128";
  let EnvView {
    line,
    vars,
    secrets,
    ..
  } = env_mode_view(&[("HTTPS_PROXY", raw)], &NetworkConfig::default())?;
  assert_eq!(
    line,
    "proxy https http://***@proxy.corp:3128 (env HTTPS_PROXY), http direct"
  );
  assert_eq!(exported(&vars, "HTTPS_PROXY"), Some(raw));
  let mut masker = SecretMasker::new();
  masker.add_secrets(&secrets);
  let echoed = format!("proxy is {raw}");
  let shown = masker.mask(&echoed);
  assert!(!shown.contains("s3cretpw"), "{shown}");
  Ok(())
}

/// B2: a password with an unencoded `/` is exported verbatim, so the whole
/// value is registered and masked.
#[test]
fn misparsed_env_proxy_is_masked_whole() -> FixtureResult<()> {
  let raw = "http://u:pa/ss9z@proxy.corp:3128";
  let EnvView { secrets, .. } = env_mode_view(&[("HTTPS_PROXY", raw)], &NetworkConfig::default())?;
  let mut masker = SecretMasker::new();
  masker.add_secrets(&secrets);
  let shown = masker.mask(raw);
  assert_eq!(shown, "***");
  Ok(())
}
