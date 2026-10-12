//! #94 AC-11 / AC-2 / AC-4: a real action archive download goes through the
//! job's `[network]` proxy and trusts its `ca_bundle` — and fails closed
//! without the bundle.

#[path = "../../network/tests/helpers/proxy_fixtures.rs"]
mod proxy_fixtures;
#[path = "../../network/tests/helpers/tls_fixtures.rs"]
mod tls_fixtures;

use std::io::Write as _;

use execution::execution::actions::downloader::{archive_client, download_and_extract_action};
use flate2::Compression;
use flate2::write::GzEncoder;
use proxy_fixtures::RecordingProxy;
use shared::{ActionFetchKind, NetworkConfig, RunnerError};
use tls_fixtures::{FixtureResult, Leaf, Pki, TlsTarget};

/// A real `owner-repo-sha/README.md` tar.gz, the shape GitHub's tarball
/// endpoint serves.
fn tarball() -> FixtureResult<Vec<u8>> {
  let body = b"served through the proxy";
  let mut raw = Vec::new();
  {
    let mut builder = tar::Builder::new(&mut raw);
    let mut header = tar::Header::new_gnu();
    header.set_size(u64::try_from(body.len())?);
    header.set_mode(0o644);
    header.set_cksum();
    builder.append_data(&mut header, "acme-probe-abc123/README.md", &body[..])?;
    builder.finish()?;
  }
  let mut gz = GzEncoder::new(Vec::new(), Compression::default());
  gz.write_all(&raw)?;
  Ok(gz.finish()?)
}

/// Every message in `error`'s source chain, joined.
fn error_chain(error: &dyn std::error::Error) -> String {
  let mut chain = vec![error.to_string()];
  let mut source = error.source();
  while let Some(inner) = source {
    chain.push(inner.to_string());
    source = inner.source();
  }
  chain.join(" <- ")
}

#[tokio::test]
async fn archive_download_uses_the_proxy_and_the_bundle() -> FixtureResult<()> {
  let pki = Pki::new()?;
  let dir = tempfile::tempdir()?;
  let target = TlsTarget::serve(
    pki.server_config(&["target.test"], Leaf::Valid)?,
    tarball()?,
  )
  .await?;
  let proxy = RecordingProxy::start(Some(("runner", "pw-94-test"))).await?;
  let url = format!(
    "https://target.test:{}/acme/probe/tarball/abc123",
    target.port()
  );

  let untrusted = NetworkConfig {
    proxy: Some(proxy.url(Some("runner:pw-94-test"))),
    no_proxy: Vec::new(),
    ca_bundle: None,
  };
  let refused = download_and_extract_action(
    &archive_client(&untrusted)?,
    &url,
    None,
    &dir.path().join("a"),
  )
  .await;
  assert!(
    matches!(
      &refused,
      Err(RunnerError::ActionFetch(error))
        if error.kind == ActionFetchKind::ArchiveTransport
          && error.message == "archive request failed: timeout=false"
    ),
    "no bundle must fail as an archive transport error: {refused:?}"
  );
  // The archive error is sanitized, so pin WHY it failed with the same
  // client builder: the TLS handshake was refused for an untrusted issuer.
  let direct = network::client_builder(&untrusted)?
    .build()?
    .get(&url)
    .send()
    .await;
  let cause = error_chain(
    &direct
      .err()
      .ok_or("untrusted request unexpectedly succeeded")?,
  );
  assert!(cause.contains("UnknownIssuer"), "{cause}");

  assert!(
    target.requests().is_empty(),
    "the refused handshake must never reach the origin: {:?}",
    target.requests()
  );
  let connect = format!("CONNECT target.test:{} HTTP/1.1", target.port());
  let refused_hits = proxy.hits();
  assert_eq!(refused_hits.len(), 2, "{refused_hits:?}");
  assert!(
    refused_hits.iter().all(|hit| hit.line == connect),
    "both untrusted attempts went through the proxy: {refused_hits:?}"
  );

  let trusted = NetworkConfig {
    ca_bundle: Some(pki.write_bundle(dir.path())?),
    ..untrusted
  };
  let dest = dir.path().join("b");
  download_and_extract_action(&archive_client(&trusted)?, &url, None, &dest).await?;
  assert_eq!(
    std::fs::read_to_string(dest.join("README.md"))?,
    "served through the proxy"
  );
  let hits = proxy.hits();
  assert_eq!(hits.len(), 3, "{hits:?}");
  assert!(hits.iter().all(|hit| hit.line == connect), "{hits:?}");
  assert_eq!(
    target.requests(),
    vec!["GET /acme/probe/tarball/abc123 HTTP/1.1".to_owned()]
  );
  Ok(())
}
