//! A recording HTTP `CONNECT` proxy for the #94 proxy tests. Included
//! beside `tls_fixtures` (it reuses its [`read_head`]) through
//! `#[path = ".../helpers/proxy_fixtures.rs"] mod proxy_fixtures;`.
//!
//! [`RecordingProxy`] (optionally) requires Basic auth, records every request
//! line with its `Proxy-Authorization` header, and tunnels to
//! `127.0.0.1:<requested port>` whatever host was asked for — so
//! `target.test` works on any OS without DNS (no `127.0.0.2`, which macOS
//! lacks). A plain-`http://` destination arrives in absolute form
//! (`GET http://host:port/path HTTP/1.1`); with valid auth it is answered
//! `200` with the body `via-proxy` instead of being forwarded.

use std::sync::{Arc, Mutex};

use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};

use super::tls_fixtures::{FixtureResult, read_head};

/// Body (`via-proxy`) the proxy answers an authenticated absolute-form request with.
const VIA_PROXY_BODY: &str = "via-proxy";

/// The value of header `name` (case-insensitive) in a request `head`.
fn header(head: &str, name: &str) -> Option<String> {
  head.lines().skip(1).find_map(|line| {
    let (key, value) = line.split_once(':')?;
    key
      .trim()
      .eq_ignore_ascii_case(name)
      .then(|| value.trim().to_owned())
  })
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

/// Answer `407` demanding Basic credentials.
async fn reject(client: &mut TcpStream) {
  let _ = client
    .write_all(
      b"HTTP/1.1 407 Proxy Authentication Required\r\nProxy-Authenticate: Basic realm=\"t\"\r\nContent-Length: 0\r\n\r\n",
    )
    .await;
}

async fn tunnel(mut client: TcpStream, sink: Arc<Mutex<Vec<ProxyHit>>>, expected: Option<String>) {
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
  let (method, target) = (parts.next(), parts.next());
  let absolute_form = method == Some("GET") && target.is_some_and(|t| t.starts_with("http://"));
  let (Some("CONNECT"), Some(authority)) = (method, target) else {
    if !absolute_form {
      let _ = client
        .write_all(b"HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\n\r\n")
        .await;
    } else if expected.is_some() && auth != expected {
      reject(&mut client).await;
    } else {
      let reply = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{VIA_PROXY_BODY}",
        VIA_PROXY_BODY.len()
      );
      let _ = client.write_all(reply.as_bytes()).await;
    }
    return;
  };
  if expected.is_some() && auth != expected {
    reject(&mut client).await;
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
