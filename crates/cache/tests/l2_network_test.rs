//! #94 AC-11: the S3 cold tier's requests go through the client it is given,
//! so the runner's `[network]` proxy / CA reach S3. The fixture target is
//! not S3: the test asserts the real `CONNECT` and the signed `PUT` that
//! arrive, not an S3 success.

#[path = "../../network/tests/helpers/proxy_fixtures.rs"]
mod proxy_fixtures;
#[path = "../../network/tests/helpers/tls_fixtures.rs"]
mod tls_fixtures;

use cache::tier::{BlobKind, L2Tier};
use proxy_fixtures::RecordingProxy;
use shared::{L2Config, NetworkConfig};
use tls_fixtures::{FixtureResult, Leaf, Pki, TlsTarget};

#[tokio::test]
async fn l2_put_goes_through_the_configured_proxy_and_ca() -> FixtureResult<()> {
  let pki = Pki::new()?;
  let dir = tempfile::tempdir()?;
  let s3 = TlsTarget::serve(
    pki.server_config(&["s3.test"], Leaf::Valid)?,
    b"ok".to_vec(),
  )
  .await?;
  let proxy = RecordingProxy::start(None).await?;
  let network = NetworkConfig {
    proxy: Some(proxy.url(None)),
    no_proxy: Vec::new(),
    ca_bundle: Some(pki.write_bundle(dir.path())?),
  };
  let client = network::client_builder(&network)?.build()?;
  let tier = L2Tier::from_config(
    &L2Config {
      bucket: "toolu-cache".to_owned(),
      endpoint: format!("https://s3.test:{}", s3.port()),
      region: "us-east-1".to_owned(),
    },
    client,
  )?;
  // Static credentials: reqsign signs locally and never probes IMDS.
  let _outcome = temp_env::async_with_vars(
    [
      ("AWS_ACCESS_KEY_ID", Some("toolu-test-access-key")),
      ("AWS_SECRET_ACCESS_KEY", Some("toolu-test-secret-key")),
      ("AWS_EC2_METADATA_DISABLED", Some("true")),
    ],
    tier.put_blob(BlobKind::Chunk, "ab12", b"payload"),
  )
  .await;
  let hits = proxy.hits();
  assert!(
    hits
      .iter()
      .any(|hit| hit.line == format!("CONNECT s3.test:{} HTTP/1.1", s3.port())),
    "{hits:?}"
  );
  assert!(
    s3.requests()
      .iter()
      .any(|line| line.starts_with("PUT /toolu-cache/chunks/ab12")),
    "{:?}",
    s3.requests()
  );
  Ok(())
}
