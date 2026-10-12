//! Real TLS peers for the #94 custom-CA tests, shared by the `network`,
//! `execution`, `cache`, `wire` and `toolu-runner` integration tests through
//! `#[path = ".../helpers/tls_fixtures.rs"] mod tls_fixtures;`.
//!
//! Everything is generated at test time — no committed keys:
//! - [`Pki`]: an rcgen CA plus leaves (valid, expired, or for another name).
//! - [`TlsTarget`]: a tokio-rustls HTTPS server answering every request with a
//!   fixed body and recording each request line.

use std::sync::{Arc, Mutex};

use rcgen::{
  BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair, KeyUsagePurpose,
  date_time_ymd,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

/// Boxed error alias for fixture setup.
pub type FixtureResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// A test certificate authority that signs leaves.
pub struct Pki {
  ca_pem: String,
  issuer: Issuer<'static, KeyPair>,
}

/// Which kind of leaf certificate a server presents.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Leaf {
  /// Valid now for the requested names.
  Valid,
  /// Signed by the CA but expired in 2001.
  Expired,
}

impl Pki {
  /// Generate a fresh CA.
  pub fn new() -> FixtureResult<Self> {
    let mut params = CertificateParams::new(Vec::<String>::new())?;
    params
      .distinguished_name
      .push(DnType::CommonName, "toolu #94 test CA");
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![
      KeyUsagePurpose::KeyCertSign,
      KeyUsagePurpose::CrlSign,
      KeyUsagePurpose::DigitalSignature,
    ];
    let key = KeyPair::generate()?;
    let cert = params.self_signed(&key)?;
    Ok(Self {
      ca_pem: cert.pem(),
      issuer: Issuer::new(params, key),
    })
  }

  /// Write the CA PEM to `<dir>/ca.pem` and return the path.
  pub fn write_bundle(&self, dir: &std::path::Path) -> FixtureResult<std::path::PathBuf> {
    let path = dir.join("ca.pem");
    std::fs::write(&path, &self.ca_pem)?;
    Ok(path)
  }

  /// A rustls server config presenting a leaf for `names`, offering only
  /// HTTP/1.1 via ALPN (so a WebSocket upgrade works and request parsing in
  /// [`TlsTarget`] stays simple).
  pub fn server_config(&self, names: &[&str], leaf: Leaf) -> FixtureResult<Arc<ServerConfig>> {
    let mut params =
      CertificateParams::new(names.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>())?;
    if leaf == Leaf::Expired {
      params.not_before = date_time_ymd(2000, 1, 1);
      params.not_after = date_time_ymd(2001, 1, 1);
    }
    let key = KeyPair::generate()?;
    let cert = params.signed_by(&key, &self.issuer)?;
    let chain = vec![CertificateDer::from(cert.der().to_vec())];
    let private = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
    let provider = Arc::new(tokio_rustls::rustls::crypto::ring::default_provider());
    let mut config = ServerConfig::builder_with_provider(provider)
      .with_safe_default_protocol_versions()?
      .with_no_client_auth()
      .with_single_cert(chain, private)?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(config))
  }
}

/// Read one HTTP request head (through the blank line) from `stream`.
pub async fn read_head<S: AsyncReadExt + Unpin>(stream: &mut S) -> std::io::Result<String> {
  let mut head = Vec::new();
  let mut byte = [0_u8; 1];
  while !head.ends_with(b"\r\n\r\n") {
    if stream.read(&mut byte).await? == 0 {
      break;
    }
    head.extend_from_slice(&byte);
  }
  Ok(String::from_utf8_lossy(&head).into_owned())
}

/// An HTTPS server on `127.0.0.1` that answers every request with `body`.
pub struct TlsTarget {
  port: u16,
  requests: Arc<Mutex<Vec<String>>>,
}

impl TlsTarget {
  /// Serve `body` (as `200 OK`) with the given TLS config.
  pub async fn serve(config: Arc<ServerConfig>, body: Vec<u8>) -> FixtureResult<Self> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&requests);
    let acceptor = TlsAcceptor::from(config);
    let body = Arc::new(body);
    tokio::spawn(async move {
      while let Ok((tcp, _)) = listener.accept().await {
        let acceptor = acceptor.clone();
        let sink = Arc::clone(&sink);
        let body = Arc::clone(&body);
        tokio::spawn(async move {
          let Ok(mut tls) = acceptor.accept(tcp).await else {
            return;
          };
          let Ok(head) = read_head(&mut tls).await else {
            return;
          };
          if let Ok(mut log) = sink.lock() {
            log.push(head.lines().next().unwrap_or_default().to_owned());
          }
          let reply = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n",
            body.len()
          );
          let _ = tls.write_all(reply.as_bytes()).await;
          let _ = tls.write_all(&body).await;
          let _ = tls.shutdown().await;
        });
      }
    });
    Ok(Self { port, requests })
  }

  /// The loopback port.
  pub fn port(&self) -> u16 {
    self.port
  }

  /// Request lines received so far (`GET /path HTTP/1.1`).
  pub fn requests(&self) -> Vec<String> {
    self.requests.lock().map(|l| l.clone()).unwrap_or_default()
  }
}
