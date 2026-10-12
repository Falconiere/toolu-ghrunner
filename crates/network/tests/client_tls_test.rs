//! #94 AC-4: `[network] ca_bundle` trust against real TLS handshakes, and the
//! fail-closed bundle rules.

#[path = "helpers/tls_fixtures.rs"]
mod tls_fixtures;

use std::error::Error as _;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use shared::{NetworkConfig, RunnerError};
use tls_fixtures::{FixtureResult, Leaf, Pki, TlsTarget};

fn bundle_cfg(path: Option<PathBuf>) -> NetworkConfig {
  NetworkConfig {
    proxy: None,
    no_proxy: Vec::new(),
    ca_bundle: path,
  }
}

/// GET `https://target.test:<port>/` pinned to loopback; `Ok(status)` or the
/// full error source chain as text.
async fn fetch(cfg: &NetworkConfig, port: u16) -> FixtureResult<Result<u16, String>> {
  let client = network::client_builder(cfg)?
    .no_proxy()
    .resolve("target.test", SocketAddr::from(([127, 0, 0, 1], port)))
    .build()?;
  Ok(
    match client
      .get(format!("https://target.test:{port}/"))
      .send()
      .await
    {
      Ok(response) => Ok(response.status().as_u16()),
      Err(e) => {
        let mut chain = e.to_string();
        let mut source = e.source();
        while let Some(s) = source {
          chain.push_str(" | ");
          chain.push_str(&s.to_string());
          source = s.source();
        }
        Err(chain)
      },
    },
  )
}

#[tokio::test]
async fn bundle_makes_the_generated_ca_trusted() -> FixtureResult<()> {
  let pki = Pki::new()?;
  let dir = tempfile::tempdir()?;
  let target = TlsTarget::serve(
    pki.server_config(&["target.test"], Leaf::Valid)?,
    b"ok".to_vec(),
  )
  .await?;
  let trusted = fetch(
    &bundle_cfg(Some(pki.write_bundle(dir.path())?)),
    target.port(),
  )
  .await?;
  assert_eq!(trusted, Ok(200));
  assert_eq!(target.requests(), vec!["GET / HTTP/1.1".to_owned()]);
  let untrusted = fetch(&bundle_cfg(None), target.port()).await?;
  let err = untrusted.err().unwrap_or_default();
  assert!(
    err.contains("invalid peer certificate: UnknownIssuer"),
    "no-bundle must fail as an unknown issuer: {err}"
  );
  assert_eq!(
    target.requests().len(),
    1,
    "the untrusted request must never reach the server"
  );
  Ok(())
}

#[tokio::test]
async fn expired_and_wrong_host_leaves_fail_even_with_the_bundle() -> FixtureResult<()> {
  let pki = Pki::new()?;
  let dir = tempfile::tempdir()?;
  let cfg = bundle_cfg(Some(pki.write_bundle(dir.path())?));
  let expired = TlsTarget::serve(
    pki.server_config(&["target.test"], Leaf::Expired)?,
    b"ok".to_vec(),
  )
  .await?;
  let wrong = TlsTarget::serve(
    pki.server_config(&["other.test"], Leaf::Valid)?,
    b"ok".to_vec(),
  )
  .await?;
  for (what, port, cause) in [
    ("expired", expired.port(), "certificate expired"),
    (
      "wrong host",
      wrong.port(),
      "certificate not valid for name \"target.test\"",
    ),
  ] {
    let outcome = fetch(&cfg, port).await?;
    let err = outcome.err().unwrap_or_default();
    assert!(
      err.contains(&format!("invalid peer certificate: {cause}")),
      "{what} must fail with `{cause}`: {err}"
    );
  }
  assert!(expired.requests().is_empty() && wrong.requests().is_empty());
  Ok(())
}

fn expect_config_error(cfg: &NetworkConfig, path: &Path, needle: &str) {
  let msg = match network::client_builder(cfg) {
    Err(RunnerError::Config(msg)) => msg,
    Err(other) => format!("expected a Config error, got {other:?}"),
    Ok(_) => "expected an error, got a client".to_owned(),
  };
  assert!(
    msg.starts_with(&format!("[network] ca_bundle {}: ", path.display())),
    "{msg}"
  );
  assert!(msg.contains(needle), "{msg} lacks {needle}");
}

#[test]
fn bad_bundles_fail_closed_naming_the_path() -> FixtureResult<()> {
  let pki = Pki::new()?;
  let dir = tempfile::tempdir()?;
  let d = dir.path();

  let relative = PathBuf::from("certs/ca.pem");
  expect_config_error(&bundle_cfg(Some(relative.clone())), &relative, "absolute");

  let missing = d.join("missing.pem");
  expect_config_error(&bundle_cfg(Some(missing.clone())), &missing, "cannot access");

  expect_config_error(&bundle_cfg(Some(d.to_path_buf())), d, "not a regular file");

  let empty = d.join("empty.pem");
  std::fs::write(&empty, "")?;
  expect_config_error(
    &bundle_cfg(Some(empty.clone())),
    &empty,
    "no PEM certificates",
  );

  let text = d.join("text.pem");
  std::fs::write(&text, "this is not PEM at all\n")?;
  expect_config_error(
    &bundle_cfg(Some(text.clone())),
    &text,
    "no PEM certificates",
  );

  let key_only = d.join("key.pem");
  std::fs::write(&key_only, rcgen::KeyPair::generate()?.serialize_pem())?;
  expect_config_error(
    &bundle_cfg(Some(key_only.clone())),
    &key_only,
    "no PEM certificates",
  );

  let ca_pem = std::fs::read_to_string(pki.write_bundle(d)?)?;
  let malformed = d.join("malformed.pem");
  std::fs::write(
    &malformed,
    format!("{ca_pem}-----BEGIN CERTIFICATE-----\n!!!not base64!!!\n-----END CERTIFICATE-----\n"),
  )?;
  expect_config_error(&bundle_cfg(Some(malformed.clone())), &malformed, "PEM");

  let not_a_cert = d.join("bad-der.pem");
  std::fs::write(
    &not_a_cert,
    "-----BEGIN CERTIFICATE-----\naGVsbG8gd29ybGQ=\n-----END CERTIFICATE-----\n",
  )?;
  expect_config_error(
    &bundle_cfg(Some(not_a_cert.clone())),
    &not_a_cert,
    "certificate",
  );
  Ok(())
}

#[test]
fn a_bundle_with_several_certificates_is_accepted() -> FixtureResult<()> {
  let first = Pki::new()?;
  let second = Pki::new()?;
  let dir = tempfile::tempdir()?;
  let first_pem = std::fs::read_to_string(first.write_bundle(dir.path())?)?;
  let second_pem = std::fs::read_to_string(second.write_bundle(dir.path())?)?;
  let path = dir.path().join("two.pem");
  std::fs::write(&path, format!("{first_pem}{second_pem}"))?;
  let described = network::describe(&bundle_cfg(Some(path.clone())))?;
  assert!(
    described.contains(&format!("ca_bundle {} (2 certificates)", path.display())),
    "{described}"
  );
  Ok(())
}
