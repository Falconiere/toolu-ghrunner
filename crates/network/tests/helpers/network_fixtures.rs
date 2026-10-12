//! Real network peers for the #94 proxy / custom-CA tests, shared by the
//! `network`, `execution`, `cache`, `wire` and `toolu-runner` integration
//! tests through `#[path]`.
//!
//! Everything is generated at test time — no committed keys:
//! - [`Pki`]: an rcgen CA plus leaves (valid, expired, or for another name).
//! - [`TlsTarget`]: a tokio-rustls HTTPS server answering every request with a
//!   fixed body and recording each request line.
//! - [`RecordingProxy`]: an HTTP `CONNECT` proxy that (optionally) requires
//!   Basic auth, records every request line with its `Proxy-Authorization`
//!   header, and tunnels to `127.0.0.1:<requested port>` whatever host was
//!   asked for — so `target.test` works on any OS without DNS (no
//!   `127.0.0.2`, which macOS lacks).

use std::sync::{Arc, Mutex};

use rcgen::{
  BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair, KeyUsagePurpose,
  date_time_ymd,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
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

  /// The CA certificate as PEM — what a `ca_bundle` file holds.
  pub fn ca_pem(&self) -> &str {
    &self.ca_pem
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
    let mut params = CertificateParams::new(names.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>())?;
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

/// The value of header `name` (case-insensitive) in a request `head`.
pub fn header(head: &str, name: &str) -> Option<String> {
  head.lines().skip(1).find_map(|line| {
    let (key, value) = line.split_once(':')?;
    key.trim().eq_ignore_ascii_case(name).then(|| value.trim().to_owned())
  })
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

  /// Serve the two bytes `ok`.
  pub async fn ok(config: Arc<ServerConfig>) -> FixtureResult<Self> {
    Self::serve(config, b"ok".to_vec()).await
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

/// One request a [`RecordingProxy`] saw.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProxyHit {
  /// The request line, e.g. `CONNECT target.test:4433 HTTP/1.1`.
  pub line: String,
  /// The `Proxy-Authorization` header, when sent.
  pub auth: Option<String>,
}

/// A recording `CONNECT` proxy on `127.0.0.1`.
pub struct RecordingProxy {
  port: u16,
  hits: Arc<Mutex<Vec<ProxyHit>>>,
}

impl RecordingProxy {
  /// Start a proxy; with `basic = Some((user, pass))` a request without the
  /// matching `Proxy-Authorization: Basic …` gets `407` and no tunnel.
  pub async fn start(basic: Option<(&str, &str)>) -> FixtureResult<Self> {
    let expected = basic.map(|(user, pass)| {
      use base64::Engine as _;
      format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"))
      )
    });
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let hits = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&hits);
    tokio::spawn(async move {
      while let Ok((client, _)) = listener.accept().await {
        let sink = Arc::clone(&sink);
        let expected = expected.clone();
        tokio::spawn(tunnel(client, sink, expected));
      }
    });
    Ok(Self { port, hits })
  }

  /// The loopback port.
  pub fn port(&self) -> u16 {
    self.port
  }

  /// `http://127.0.0.1:<port>`, optionally with `user:pass@` userinfo
  /// (`pass` is inserted verbatim, so percent-encode it yourself).
  pub fn url(&self, userinfo: Option<&str>) -> String {
    match userinfo {
      Some(info) => format!("http://{info}@127.0.0.1:{}", self.port),
      None => format!("http://127.0.0.1:{}", self.port),
    }
  }

  /// Every request seen so far.
  pub fn hits(&self) -> Vec<ProxyHit> {
    self.hits.lock().map(|h| h.clone()).unwrap_or_default()
  }
}

async fn tunnel(
  mut client: TcpStream,
  sink: Arc<Mutex<Vec<ProxyHit>>>,
  expected: Option<String>,
) {
  let Ok(head) = read_head(&mut client).await else {
    return;
  };
  let line = head.lines().next().unwrap_or_default().to_owned();
  let auth = header(&head, "Proxy-Authorization");
  if let Ok(mut log) = sink.lock() {
    log.push(ProxyHit {
      line: line.clone(),
      auth: auth.clone(),
    });
  }
  let mut parts = line.split_whitespace();
  let (Some("CONNECT"), Some(authority)) = (parts.next(), parts.next()) else {
    let _ = client
      .write_all(b"HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\n\r\n")
      .await;
    return;
  };
  if expected.is_some() && auth != expected {
    let _ = client
      .write_all(
        b"HTTP/1.1 407 Proxy Authentication Required\r\nProxy-Authenticate: Basic realm=\"t\"\r\nContent-Length: 0\r\n\r\n",
      )
      .await;
    return;
  }
  let port = authority.rsplit_once(':').map_or("443", |(_, p)| p);
  let Ok(mut upstream) = TcpStream::connect(format!("127.0.0.1:{port}")).await else {
    let _ = client.write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n").await;
    return;
  };
  if client
    .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
    .await
    .is_err()
  {
    return;
  }
  let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
}

/// A loopback port nothing listens on.
pub async fn closed_port() -> FixtureResult<u16> {
  let listener = TcpListener::bind("127.0.0.1:0").await?;
  Ok(listener.local_addr()?.port())
}
