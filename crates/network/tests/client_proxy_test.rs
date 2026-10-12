//! #94 AC-2 / AC-3: `network::client_builder` proxy routing against a real
//! recording `CONNECT` proxy and a real rustls HTTPS target.

#[path = "helpers/network_fixtures.rs"]
mod network_fixtures;

use std::net::SocketAddr;

use network_fixtures::{FixtureResult, Leaf, Pki, RecordingProxy, TlsTarget, closed_port};
use shared::{NetworkConfig, RunnerError};

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
  let target = TlsTarget::ok(e.pki.server_config(&["target.test"], Leaf::Valid)?).await?;
  let proxy = RecordingProxy::start(Some(("u", "p@ss"))).await?;
  let cfg = NetworkConfig {
    proxy: Some(proxy.url(Some("u:p%40ss"))),
    no_proxy: Vec::new(),
    ca_bundle: Some(e.pki.write_bundle(e.dir.path())?),
  };
  let client = client_with_env(&cfg, &[], None)?;
  let body = get(&client, &format!("https://target.test:{}/x", target.port())).await?;
  assert_eq!(body, "ok");
  let hits = proxy.hits();
  assert_eq!(hits.len(), 1, "{hits:?}");
  assert_eq!(
    hits[0].line,
    format!("CONNECT target.test:{} HTTP/1.1", target.port())
  );
  assert_eq!(hits[0].auth.as_deref(), Some(basic("u", "p@ss").as_str()));
  assert_eq!(target.requests(), vec!["GET /x HTTP/1.1".to_owned()]);
  Ok(())
}

#[tokio::test]
async fn no_proxy_destination_goes_direct() -> FixtureResult<()> {
  let e = env()?;
  let target = TlsTarget::ok(e.pki.server_config(&["bypass.test"], Leaf::Valid)?).await?;
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
  let target = TlsTarget::ok(e.pki.server_config(&["target.test"], Leaf::Valid)?).await?;
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
  let target = TlsTarget::ok(e.pki.server_config(&["bypass.test"], Leaf::Valid)?).await?;
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
  let target = TlsTarget::ok(e.pki.server_config(&["target.test"], Leaf::Valid)?).await?;
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
  let target = TlsTarget::ok(e.pki.server_config(&["127.0.0.1"], Leaf::Valid)?).await?;
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
    match network::client_builder(&cfg) {
      Err(RunnerError::Config(msg)) => {
        assert!(msg.contains("[network] proxy"), "{msg}");
        assert!(!msg.contains("secretpw"), "{msg}");
      },
      Err(other) => panic!("{raw}: expected Config error, got {other:?}"),
      Ok(_) => panic!("{raw}: expected an error"),
    }
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
