//! #94 AC-4: `[network] ca_bundle` trust against real TLS handshakes, and the
//! fail-closed bundle rules.

#[path = "helpers/network_fixtures.rs"]
mod network_fixtures;

use std::error::Error as _;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use network_fixtures::{FixtureResult, Leaf, Pki, TlsTarget};
use shared::{NetworkConfig, RunnerError};

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
  let target = TlsTarget::ok(pki.server_config(&["target.test"], Leaf::Valid)?).await?;
  let trusted = fetch(&bundle_cfg(Some(pki.write_bundle(dir.path())?)), target.port()).await?;
  assert_eq!(trusted, Ok(200));
  let untrusted = fetch(&bundle_cfg(None), target.port()).await?;
  let err = untrusted.err().unwrap_or_default();
  assert!(err.contains("certificate"), "no-bundle must fail TLS: {err}");
  Ok(())
}

#[tokio::test]
async fn expired_and_wrong_host_leaves_fail_even_with_the_bundle() -> FixtureResult<()> {
  let pki = Pki::new()?;
  let dir = tempfile::tempdir()?;
  let cfg = bundle_cfg(Some(pki.write_bundle(dir.path())?));
  let expired = TlsTarget::ok(pki.server_config(&["target.test"], Leaf::Expired)?).await?;
  let wrong = TlsTarget::ok(pki.server_config(&["other.test"], Leaf::Valid)?).await?;
  for (what, port) in [("expired", expired.port()), ("wrong host", wrong.port())] {
    let outcome = fetch(&cfg, port).await?;
    let err = outcome.err().unwrap_or_default();
    assert!(err.contains("certificate"), "{what} must fail TLS: {err}");
  }
  Ok(())
}

fn expect_config_error(cfg: &NetworkConfig, path: &Path, needle: &str) {
  match network::client_builder(cfg) {
    Err(RunnerError::Config(msg)) => {
      assert!(msg.contains(&path.display().to_string()), "{msg}");
      assert!(msg.contains(needle), "{msg} lacks {needle}");
    },
    Err(other) => panic!("{}: expected Config, got {other:?}", path.display()),
    Ok(_) => panic!("{}: expected an error", path.display()),
  }
}

#[test]
fn bad_bundles_fail_closed_naming_the_path() -> FixtureResult<()> {
  let pki = Pki::new()?;
  let dir = tempfile::tempdir()?;
  let d = dir.path();

  let relative = PathBuf::from("certs/ca.pem");
  expect_config_error(&bundle_cfg(Some(relative.clone())), &relative, "absolute");

  let missing = d.join("missing.pem");
  expect_config_error(&bundle_cfg(Some(missing.clone())), &missing, "cannot read");

  expect_config_error(&bundle_cfg(Some(d.to_path_buf())), d, "not a regular file");

  let empty = d.join("empty.pem");
  std::fs::write(&empty, "")?;
  expect_config_error(&bundle_cfg(Some(empty.clone())), &empty, "no PEM certificates");

  let text = d.join("text.pem");
  std::fs::write(&text, "this is not PEM at all\n")?;
  expect_config_error(&bundle_cfg(Some(text.clone())), &text, "no PEM certificates");

  let key_only = d.join("key.pem");
  std::fs::write(&key_only, rcgen::KeyPair::generate()?.serialize_pem())?;
  expect_config_error(&bundle_cfg(Some(key_only.clone())), &key_only, "no PEM certificates");

  let malformed = d.join("malformed.pem");
  std::fs::write(
    &malformed,
    format!(
      "{}-----BEGIN CERTIFICATE-----\n!!!not base64!!!\n-----END CERTIFICATE-----\n",
      pki.ca_pem()
    ),
  )?;
  expect_config_error(&bundle_cfg(Some(malformed.clone())), &malformed, "PEM");

  let not_a_cert = d.join("bad-der.pem");
  std::fs::write(
    &not_a_cert,
    "-----BEGIN CERTIFICATE-----\naGVsbG8gd29ybGQ=\n-----END CERTIFICATE-----\n",
  )?;
  expect_config_error(&bundle_cfg(Some(not_a_cert.clone())), &not_a_cert, "certificate");
  Ok(())
}

#[test]
fn a_bundle_with_several_certificates_is_accepted() -> FixtureResult<()> {
  let first = Pki::new()?;
  let second = Pki::new()?;
  let dir = tempfile::tempdir()?;
  let path = dir.path().join("two.pem");
  std::fs::write(&path, format!("{}{}", first.ca_pem(), second.ca_pem()))?;
  let described = network::describe(&bundle_cfg(Some(path.clone())))?;
  assert!(
    described.contains(&format!("ca_bundle {} (2 certificates)", path.display())),
    "{described}"
  );
  Ok(())
}
