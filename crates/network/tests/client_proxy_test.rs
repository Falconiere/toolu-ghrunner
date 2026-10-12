//! #94 AC-2 / AC-3: `network::client_builder` proxy routing against a real
//! recording `CONNECT` proxy and a real rustls HTTPS target.

#[path = "helpers/proxy_fixtures.rs"]
mod proxy_fixtures;
#[path = "helpers/tls_fixtures.rs"]
mod tls_fixtures;

use std::net::SocketAddr;

use proxy_fixtures::RecordingProxy;
use shared::{NetworkConfig, RunnerError};
use tls_fixtures::{FixtureResult, Leaf, Pki, TlsTarget};

/// A loopback port nothing listens on.
async fn closed_port() -> FixtureResult<u16> {
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
  Ok(listener.local_addr()?.port())
}

/// Every proxy variable reqwest's system matcher reads, unset — the host
/// running the tests may export some.
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

/// Build a client from `cfg` with `env` applied (and every other proxy var
/// unset) — reqwest reads the system proxy at `build()` time.
fn client_with_env(
  cfg: &NetworkConfig,
  env: &[(&str, &str)],
  resolve: Option<(&str, u16)>,
) -> Result<reqwest::Client, RunnerError> {
  let vars: Vec<(&str, Option<&str>)> = PROXY_VARS
    .iter()
    .map(|name| (*name, env.iter().find(|(k, _)| k == name).map(|(_, v)| *v)))
    .collect();
  temp_env::with_vars(vars, || {
    let mut builder = network::client_builder(cfg)?;
    if let Some((host, port)) = resolve {
      builder = builder.resolve(host, SocketAddr::from(([127, 0, 0, 1], port)));
    }
    builder
      .build()
      .map_err(|e| RunnerError::Network(e.to_string()))
  })
}

async fn get(client: &reqwest::Client, url: &str) -> FixtureResult<String> {
  let response = client.get(url).send().await?;
  assert_eq!(response.status(), 200, "{url}");
  Ok(response.text().await?)
}

struct Env {
  pki: Pki,
  dir: tempfile::TempDir,
}

fn env() -> FixtureResult<Env> {
  Ok(Env {
    pki: Pki::new()?,
    dir: tempfile::tempdir()?,
  })
}

fn basic(user: &str, pass: &str) -> String {
  use base64::Engine as _;
  format!(
    "Basic {}",
    base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"))
  )
}

#[tokio::test]
async fn config_proxy_tunnels_with_decoded_basic_auth() -> FixtureResult<()> {
  let e = env()?;
  let target = TlsTarget::serve(
    e.pki.server_config(&["target.test"], Leaf::Valid)?,
    b"ok".to_vec(),
  )
  .await?;
  let proxy = RecordingProxy::start(Some(("u", "p@ss"))).await?;
  let cfg = NetworkConfig {
    proxy: Some(proxy.url(Some("u:p%40ss"))),
    no_proxy: Vec::new(),
    ca_bundle: Some(e.pki.write_bundle(e.dir.path())?),
  };
  let client = client_with_env(&cfg, &[], None)?;
  let body = get(&client, &format!("https://target.test:{}/x", target.port())).await?;
  assert_eq!(body, "ok");
  let line = format!("CONNECT target.test:{} HTTP/1.1", target.port());
  let auth = basic("u", "p@ss");
  let hits = proxy.hits();
  assert_eq!(
    hits
      .iter()
      .map(|hit| (hit.line.as_str(), hit.auth.as_deref()))
      .collect::<Vec<_>>(),
    vec![(line.as_str(), Some(auth.as_str()))]
  );
  assert_eq!(target.requests(), vec!["GET /x HTTP/1.1".to_owned()]);
  Ok(())
}

#[tokio::test]
async fn no_proxy_destination_goes_direct() -> FixtureResult<()> {
  let e = env()?;
  let target = TlsTarget::serve(
    e.pki.server_config(&["bypass.test"], Leaf::Valid)?,
    b"ok".to_vec(),
  )
  .await?;
  let proxy = RecordingProxy::start(None).await?;
  let cfg = NetworkConfig {
    proxy: Some(proxy.url(None)),
    no_proxy: vec!["internal.corp".to_owned(), "bypass.test".to_owned()],
    ca_bundle: Some(e.pki.write_bundle(e.dir.path())?),
  };
  let client = client_with_env(&cfg, &[], Some(("bypass.test", target.port())))?;
  get(&client, &format!("https://bypass.test:{}/", target.port())).await?;
  assert!(proxy.hits().is_empty(), "{:?}", proxy.hits());
  assert_eq!(target.requests().len(), 1);
  Ok(())
}

#[tokio::test]
async fn config_proxy_replaces_env_proxy_and_env_no_proxy() -> FixtureResult<()> {
  let e = env()?;
  let target = TlsTarget::serve(
    e.pki.server_config(&["target.test"], Leaf::Valid)?,
    b"ok".to_vec(),
  )
  .await?;
  let configured = RecordingProxy::start(None).await?;
  let ambient = RecordingProxy::start(None).await?;
  let cfg = NetworkConfig {
    proxy: Some(configured.url(None)),
    no_proxy: Vec::new(),
    ca_bundle: Some(e.pki.write_bundle(e.dir.path())?),
  };
  let ambient_url = ambient.url(None);
  let client = client_with_env(
    &cfg,
    &[("HTTPS_PROXY", &ambient_url), ("NO_PROXY", "target.test")],
    None,
  )?;
  get(&client, &format!("https://target.test:{}/", target.port())).await?;
  assert_eq!(configured.hits().len(), 1, "config proxy must carry it");
  assert!(ambient.hits().is_empty(), "env proxy must be ignored");
  Ok(())
}

#[tokio::test]
async fn config_no_proxy_is_direct_even_with_a_dead_env_proxy() -> FixtureResult<()> {
  let e = env()?;
  let target = TlsTarget::serve(
    e.pki.server_config(&["bypass.test"], Leaf::Valid)?,
    b"ok".to_vec(),
  )
  .await?;
  let configured = RecordingProxy::start(None).await?;
  let dead = format!("http://127.0.0.1:{}", closed_port().await?);
  let cfg = NetworkConfig {
    proxy: Some(configured.url(None)),
    no_proxy: vec!["bypass.test".to_owned()],
    ca_bundle: Some(e.pki.write_bundle(e.dir.path())?),
  };
  let client = client_with_env(
    &cfg,
    &[("HTTPS_PROXY", &dead), ("https_proxy", &dead)],
    Some(("bypass.test", target.port())),
  )?;
  get(&client, &format!("https://bypass.test:{}/", target.port())).await?;
  assert!(configured.hits().is_empty());
  Ok(())
}

#[tokio::test]
async fn env_mode_keeps_the_env_proxy() -> FixtureResult<()> {
  let e = env()?;
  let target = TlsTarget::serve(
    e.pki.server_config(&["target.test"], Leaf::Valid)?,
    b"ok".to_vec(),
  )
  .await?;
  let ambient = RecordingProxy::start(None).await?;
  let cfg = NetworkConfig {
    proxy: None,
    no_proxy: vec!["target.test".to_owned()], // ignored without `proxy`
    ca_bundle: Some(e.pki.write_bundle(e.dir.path())?),
  };
  let ambient_url = ambient.url(None);
  let client = client_with_env(&cfg, &[("HTTPS_PROXY", &ambient_url)], None)?;
  get(&client, &format!("https://target.test:{}/", target.port())).await?;
  assert_eq!(ambient.hits().len(), 1);
  Ok(())
}

#[tokio::test]
async fn loopback_bypasses_a_configured_proxy() -> FixtureResult<()> {
  let e = env()?;
  let target = TlsTarget::serve(
    e.pki.server_config(&["127.0.0.1"], Leaf::Valid)?,
    b"ok".to_vec(),
  )
  .await?;
  let dead = format!("http://127.0.0.1:{}", closed_port().await?);
  let cfg = NetworkConfig {
    proxy: Some(dead),
    no_proxy: Vec::new(),
    ca_bundle: Some(e.pki.write_bundle(e.dir.path())?),
  };
  let client = client_with_env(&cfg, &[], None)?;
  get(&client, &format!("https://127.0.0.1:{}/", target.port())).await?;
  assert_eq!(network::LOOPBACK_BYPASS, ["localhost", "127.0.0.1", "::1"]);
  Ok(())
}

#[test]
fn invalid_proxy_values_fail_closed_with_the_credential_redacted() {
  for raw in [
    "socks5://u:secretpw@proxy:1080",
    "u:secretpw@proxy:3128",
    "http://u:secretpw@",
    "not a url",
  ] {
    let cfg = NetworkConfig {
      proxy: Some(raw.to_owned()),
      ..NetworkConfig::default()
    };
    let msg = match network::client_builder(&cfg) {
      Err(RunnerError::Config(msg)) => msg,
      Err(other) => format!("{raw}: expected a Config error, got {other:?}"),
      Ok(_) => format!("{raw}: expected an error, got a client"),
    };
    assert!(msg.starts_with("[network] proxy"), "{msg}");
    assert!(!msg.contains("secretpw"), "{msg}");
  }
}

#[test]
fn blank_proxy_is_env_mode() {
  let cfg = NetworkConfig {
    proxy: Some("  ".to_owned()),
    ..NetworkConfig::default()
  };
  assert!(network::client_builder(&cfg).is_ok());
}

#[tokio::test]
async fn plain_http_destination_is_sent_in_absolute_form_with_auth() -> FixtureResult<()> {
  let proxy = RecordingProxy::start(Some(("u", "p@ss"))).await?;
  let cfg = NetworkConfig {
    proxy: Some(proxy.url(Some("u:p%40ss"))),
    ..NetworkConfig::default()
  };
  let client = client_with_env(&cfg, &[], None)?;
  let body = get(&client, "http://plain.test:8080/some/path?x=1").await?;
  assert_eq!(body, "via-proxy");
  let auth = basic("u", "p@ss");
  assert_eq!(
    proxy
      .hits()
      .iter()
      .map(|hit| (hit.line.as_str(), hit.auth.as_deref()))
      .collect::<Vec<_>>(),
    vec![(
      "GET http://plain.test:8080/some/path?x=1 HTTP/1.1",
      Some(auth.as_str())
    )]
  );
  Ok(())
}

#[tokio::test]
async fn plain_http_destination_without_credentials_is_rejected_with_407() -> FixtureResult<()> {
  let proxy = RecordingProxy::start(Some(("u", "pw"))).await?;
  let cfg = NetworkConfig {
    proxy: Some(proxy.url(None)),
    ..NetworkConfig::default()
  };
  let client = client_with_env(&cfg, &[], None)?;
  let response = client.get("http://plain.test:8080/").send().await?;
  assert_eq!(response.status(), 407);
  Ok(())
}

#[tokio::test]
async fn empty_username_with_a_password_still_sends_basic_auth() -> FixtureResult<()> {
  let proxy = RecordingProxy::start(Some(("", "pw"))).await?;
  let cfg = NetworkConfig {
    proxy: Some(proxy.url(Some(":pw"))),
    ..NetworkConfig::default()
  };
  assert!(
    cfg
      .proxy
      .as_deref()
      .is_some_and(|p| p.starts_with("http://:pw@"))
  );
  let client = client_with_env(&cfg, &[], None)?;
  let body = get(&client, "http://plain.test:8080/").await?;
  assert_eq!(body, "via-proxy");
  let auth = basic("", "pw");
  let hits = proxy.hits();
  assert_eq!(hits.len(), 1, "{hits:?}");
  assert_eq!(
    hits.first().and_then(|h| h.auth.as_deref()),
    Some(auth.as_str())
  );
  Ok(())
}

/// The `[network] proxy` Config error for `raw`, or a description of why
/// there was none.
fn proxy_error(raw: &str) -> String {
  let cfg = NetworkConfig {
    proxy: Some(raw.to_owned()),
    ..NetworkConfig::default()
  };
  match network::client_builder(&cfg) {
    Err(RunnerError::Config(msg)) => msg,
    Err(other) => format!("{raw}: expected a Config error, got {other:?}"),
    Ok(_) => format!("{raw}: expected an error, got a client"),
  }
}

#[test]
fn reserved_characters_in_a_password_are_rejected_without_leaking_it() {
  const HINT: &str = "percent-encode reserved characters (@ : / ? #) in the user name and password";
  for (raw, expected) in [
    // Parse as a bad port: the host would be the user name.
    (
      "http://u:pwd9z#cd@proxy:3128",
      "[network] proxy http://u:***@proxy:3128: not a valid URL (invalid port number)".to_owned(),
    ),
    (
      "http://u:pwd9z/b@proxy",
      "[network] proxy http://u:***@proxy: not a valid URL (invalid port number)".to_owned(),
    ),
    // Parse "successfully" with the `@` in the fragment / path / query.
    (
      "http://u:123#pwd9z@proxy:3128",
      format!("[network] proxy http://u:***@proxy:3128: {HINT}"),
    ),
    (
      "http://u:123/pwd9z@proxy",
      format!("[network] proxy http://u:***@proxy: {HINT}"),
    ),
    (
      "http://u:123?pwd9z@proxy",
      format!("[network] proxy http://u:***@proxy: {HINT}"),
    ),
  ] {
    let msg = proxy_error(raw);
    assert_eq!(msg, expected, "{raw}");
    assert!(!msg.contains("pwd9z"), "{msg}");
  }
}
