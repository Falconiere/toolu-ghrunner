//! #94: the job's HTTP clients go through the configured proxy and trust its
//! CA bundle, and the container env export keeps explicit values. Real TLS
//! target, real recording proxy, real specs — no mocks.

#[path = "../../../../../network/tests/helpers/proxy_fixtures.rs"]
mod proxy_fixtures;
#[path = "../../../../../network/tests/helpers/tls_fixtures.rs"]
mod tls_fixtures;

use std::collections::HashMap;
use std::path::PathBuf;

use base64::Engine as _;
use proxy_fixtures::RecordingProxy;
use shared::{NetworkConfig, RunnerConfig};
use tls_fixtures::{FixtureResult, Leaf, Pki, TlsTarget};

use super::{accelerated_upstream_client, add_container_network_env, build_job_client};
use crate::docker::container_spec::ContainerSpec;
use crate::docker::service_spec::ServiceSpec;

const PROXY_USER: &str = "runner";
const PROXY_PASS: &str = "pw-94-test";

/// Both clients must send a TLS request to a CA-signed origin through the
/// authenticated proxy as one `CONNECT`, then read the origin's body.
async fn assert_client_uses_proxy_and_bundle(
  build: fn(&RunnerConfig) -> Result<reqwest::Client, shared::RunnerError>,
) -> FixtureResult<()> {
  let pki = Pki::new()?;
  let dir = tempfile::tempdir()?;
  let target = TlsTarget::serve(
    pki.server_config(&["target.test"], Leaf::Valid)?,
    b"origin-body".to_vec(),
  )
  .await?;
  let proxy = RecordingProxy::start(Some((PROXY_USER, PROXY_PASS))).await?;
  let config = RunnerConfig {
    network: NetworkConfig {
      proxy: Some(proxy.url(Some(&format!("{PROXY_USER}:{PROXY_PASS}")))),
      no_proxy: Vec::new(),
      ca_bundle: Some(pki.write_bundle(dir.path())?),
    },
    ..RunnerConfig::default()
  };
  let client = build(&config)?;

  let response = client
    .get(format!("https://target.test:{}/", target.port()))
    .send()
    .await?;
  assert_eq!(response.status(), reqwest::StatusCode::OK);
  assert_eq!(response.text().await?, "origin-body");

  let expected_auth = format!(
    "Basic {}",
    base64::engine::general_purpose::STANDARD.encode(format!("{PROXY_USER}:{PROXY_PASS}"))
  );
  let hits = proxy.hits();
  assert_eq!(hits.len(), 1, "{hits:?}");
  assert_eq!(
    hits.first().map(|hit| hit.line.as_str()),
    Some(format!("CONNECT target.test:{} HTTP/1.1", target.port()).as_str())
  );
  assert_eq!(
    hits.first().and_then(|hit| hit.auth.as_deref()),
    Some(expected_auth.as_str())
  );
  assert_eq!(target.requests(), vec!["GET / HTTP/1.1".to_owned()]);
  Ok(())
}

#[tokio::test]
async fn job_client_goes_through_the_configured_proxy_and_ca() -> FixtureResult<()> {
  assert_client_uses_proxy_and_bundle(build_job_client).await
}

#[tokio::test]
async fn accelerated_upstream_client_goes_through_the_configured_proxy_and_ca() -> FixtureResult<()>
{
  assert_client_uses_proxy_and_bundle(accelerated_upstream_client).await
}

const PROXY_NAMES: [&str; 6] = [
  "HTTPS_PROXY",
  "https_proxy",
  "HTTP_PROXY",
  "http_proxy",
  "NO_PROXY",
  "no_proxy",
];
const CONFIGURED_PROXY: &str = "http://u:pw-94@proxy.corp.test:3128";

fn spec(env: &[(&str, &str)]) -> ContainerSpec {
  ContainerSpec {
    image: "alpine".to_owned(),
    credentials: None,
    env: env
      .iter()
      .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
      .collect(),
    ports: Vec::new(),
    volumes: Vec::new(),
    options: Vec::new(),
  }
}

fn service(alias: &str, env: &[(&str, &str)]) -> ServiceSpec {
  ServiceSpec {
    alias: alias.to_owned(),
    container: spec(env),
  }
}

#[test]
fn container_env_keeps_explicit_values_and_never_gets_the_host_ca_path() {
  let config = RunnerConfig {
    network: NetworkConfig {
      proxy: Some(CONFIGURED_PROXY.to_owned()),
      no_proxy: vec!["internal.test".to_owned()],
      ca_bundle: Some(PathBuf::from("/etc/ssl/corp-ca.pem")),
    },
    ..RunnerConfig::default()
  };
  let mut job = spec(&[("HTTP_PROXY", "http://own-job.test:1")]);
  let mut services = vec![
    service("db", &[("HTTP_PROXY", "http://own-svc.test:2")]),
    service("cache", &[]),
  ];

  add_container_network_env(&config, Some(&mut job), &mut services);

  let no_proxy = "internal.test,localhost,127.0.0.1,::1";
  let mut expected_job: HashMap<String, String> = HashMap::new();
  for name in ["HTTPS_PROXY", "https_proxy", "http_proxy"] {
    expected_job.insert(name.to_owned(), CONFIGURED_PROXY.to_owned());
  }
  expected_job.insert("HTTP_PROXY".to_owned(), "http://own-job.test:1".to_owned());
  for name in ["NO_PROXY", "no_proxy"] {
    expected_job.insert(name.to_owned(), no_proxy.to_owned());
  }
  assert_eq!(job.env, expected_job);

  let mut expected_db = expected_job.clone();
  expected_db.insert("HTTP_PROXY".to_owned(), "http://own-svc.test:2".to_owned());
  assert_eq!(
    services.first().map(|s| &s.container.env),
    Some(&expected_db)
  );

  let mut expected_cache = expected_job;
  expected_cache.insert("HTTP_PROXY".to_owned(), CONFIGURED_PROXY.to_owned());
  assert_eq!(
    services.get(1).map(|s| &s.container.env),
    Some(&expected_cache)
  );
  assert!(
    job
      .env
      .keys()
      .chain(services.iter().flat_map(|s| s.container.env.keys()))
      .all(|key| key != "NODE_EXTRA_CA_CERTS")
  );
}

#[test]
fn empty_network_in_env_mode_without_proxy_env_adds_nothing() {
  let cleared: Vec<(&str, Option<&str>)> = PROXY_NAMES.iter().map(|name| (*name, None)).collect();
  temp_env::with_vars(cleared, || {
    let mut job = spec(&[]);
    let mut services = vec![service("db", &[("KEEP", "1")])];
    add_container_network_env(&RunnerConfig::default(), Some(&mut job), &mut services);
    assert!(job.env.is_empty(), "{:?}", job.env);
    assert_eq!(
      services.first().map(|s| s.container.env.clone()),
      Some(HashMap::from([("KEEP".to_owned(), "1".to_owned())]))
    );
  });
}
