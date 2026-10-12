//! #94 AC-8: the live-log WebSocket rides the shared `network` client — a
//! real `wss://` server behind a real authenticated CONNECT proxy, trusted
//! only through the generated `ca_bundle`.

#[path = "../../network/tests/helpers/proxy_fixtures.rs"]
mod proxy_fixtures;
#[path = "../../network/tests/helpers/tls_fixtures.rs"]
mod tls_fixtures;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt as _;
use proxy_fixtures::RecordingProxy;
use shared::NetworkConfig;
use tls_fixtures::{FixtureResult, Leaf, Pki, TlsTarget, read_head};
use tokio::io::AsyncWriteExt as _;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::ServerConfig;
use tokio_tungstenite::tungstenite::handshake::server::{
  Callback, ErrorResponse, Request, Response,
};
use wire::reporting::live_log::{LiveLogLine, LiveLogStreamer, live_log_client};

/// What a TLS WebSocket server saw: the `Authorization` header and every
/// text frame.
#[derive(Default)]
struct Seen {
  auth: Option<String>,
  frames: Vec<String>,
}

/// Handshake callback recording the client's `Authorization` header.
struct RecordAuth(Arc<Mutex<Seen>>);

impl Callback for RecordAuth {
  fn on_request(self, request: &Request, response: Response) -> Result<Response, ErrorResponse> {
    if let Ok(mut s) = self.0.lock() {
      s.auth = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    }
    Ok(response)
  }
}

/// A `wss://` server on loopback that records one connection.
async fn wss_server(config: Arc<ServerConfig>) -> FixtureResult<(u16, Arc<Mutex<Seen>>)> {
  let listener = TcpListener::bind("127.0.0.1:0").await?;
  let port = listener.local_addr()?.port();
  let seen = Arc::new(Mutex::new(Seen::default()));
  let sink = Arc::clone(&seen);
  let acceptor = TlsAcceptor::from(config);
  tokio::spawn(async move {
    while let Ok((tcp, _)) = listener.accept().await {
      let Ok(tls) = acceptor.accept(tcp).await else {
        continue;
      };
      let callback = RecordAuth(Arc::clone(&sink));
      let Ok(mut ws) = tokio_tungstenite::accept_hdr_async(tls, callback).await else {
        continue;
      };
      while let Some(Ok(message)) = ws.next().await {
        if let Ok(text) = message.into_text()
          && let Ok(mut s) = sink.lock()
        {
          s.frames.push(text.to_string());
        }
      }
    }
  });
  Ok((port, seen))
}

/// A TLS server that answers any request `101 Switching Protocols` with the
/// given `Sec-WebSocket-Accept` header value (`None` omits the header), and
/// records each request line.
async fn fake_upgrade_server(
  config: Arc<ServerConfig>,
  accept: Option<&'static str>,
) -> FixtureResult<(u16, Arc<Mutex<Vec<String>>>)> {
  let listener = TcpListener::bind("127.0.0.1:0").await?;
  let port = listener.local_addr()?.port();
  let requests = Arc::new(Mutex::new(Vec::new()));
  let sink = Arc::clone(&requests);
  let acceptor = TlsAcceptor::from(config);
  tokio::spawn(async move {
    while let Ok((tcp, _)) = listener.accept().await {
      let Ok(mut tls) = acceptor.accept(tcp).await else {
        continue;
      };
      let Ok(head) = read_head(&mut tls).await else {
        continue;
      };
      if let Ok(mut log) = sink.lock() {
        log.push(head.lines().next().unwrap_or_default().to_owned());
      }
      let accept_line =
        accept.map_or_else(String::new, |v| format!("Sec-WebSocket-Accept: {v}\r\n"));
      let reply = format!(
        "HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n{accept_line}\r\n"
      );
      let _ = tls.write_all(reply.as_bytes()).await;
      let _ = tls.shutdown().await;
    }
  });
  Ok((port, requests))
}

#[tokio::test]
async fn live_log_tunnels_through_the_proxy_with_the_custom_ca() -> FixtureResult<()> {
  let pki = Pki::new()?;
  let dir = tempfile::tempdir()?;
  let (port, seen) = wss_server(pki.server_config(&["target.test"], Leaf::Valid)?).await?;
  let proxy = RecordingProxy::start(Some(("u", "s3cret"))).await?;
  let cfg = NetworkConfig {
    proxy: Some(proxy.url(Some("u:s3cret"))),
    no_proxy: Vec::new(),
    ca_bundle: Some(pki.write_bundle(dir.path())?),
  };
  let client = live_log_client(&cfg)?;
  let url = format!("wss://target.test:{port}/feed/abc");
  let Some((tx, handle)) = LiveLogStreamer::connect(&client, &url, "feed-token").await else {
    return Err("connect returned None through the proxy".into());
  };
  tx.send(LiveLogLine {
    step_id: "step-1".to_owned(),
    line: "hello through the proxy".to_owned(),
  })
  .await?;
  drop(tx);
  tokio::time::timeout(Duration::from_secs(10), handle).await??;
  // The server task records frames as they arrive; give it a moment.
  for _ in 0..50 {
    if seen.lock().map(|s| !s.frames.is_empty()).unwrap_or(false) {
      break;
    }
    tokio::time::sleep(Duration::from_millis(20)).await;
  }
  let lines: Vec<String> = proxy.hits().into_iter().map(|hit| hit.line).collect();
  assert_eq!(lines, vec![format!("CONNECT target.test:{port} HTTP/1.1")]);
  let s = seen.lock().map_err(|e| e.to_string())?;
  assert_eq!(s.auth.as_deref(), Some("Bearer feed-token"));
  assert!(
    s.frames
      .iter()
      .any(|f| f.contains("hello through the proxy")),
    "{:?}",
    s.frames
  );
  Ok(())
}

#[tokio::test]
async fn live_log_refuses_an_untrusted_server() -> FixtureResult<()> {
  let pki = Pki::new()?;
  let (port, seen) = wss_server(pki.server_config(&["target.test"], Leaf::Valid)?).await?;
  let proxy = RecordingProxy::start(None).await?;
  let cfg = NetworkConfig {
    proxy: Some(proxy.url(None)),
    no_proxy: Vec::new(),
    ca_bundle: None,
  };
  let url = format!("wss://target.test:{port}/feed");
  let connected = LiveLogStreamer::connect(&live_log_client(&cfg)?, &url, "t").await;
  assert!(
    connected.is_none(),
    "TLS to an untrusted CA must fail closed"
  );
  assert!(seen.lock().map(|s| s.frames.is_empty()).unwrap_or(false));
  Ok(())
}

#[tokio::test]
async fn live_log_rejects_a_server_that_does_not_upgrade() -> FixtureResult<()> {
  let pki = Pki::new()?;
  let dir = tempfile::tempdir()?;
  let plain = TlsTarget::serve(
    pki.server_config(&["target.test"], Leaf::Valid)?,
    b"ok".to_vec(),
  )
  .await?;
  let proxy = RecordingProxy::start(None).await?;
  let cfg = NetworkConfig {
    proxy: Some(proxy.url(None)),
    no_proxy: Vec::new(),
    ca_bundle: Some(pki.write_bundle(dir.path())?),
  };
  let url = format!("wss://target.test:{}/feed", plain.port());
  let connected = LiveLogStreamer::connect(&live_log_client(&cfg)?, &url, "t").await;
  assert!(connected.is_none(), "a 200 answer is not a WebSocket");
  assert_eq!(plain.requests(), vec!["GET /feed HTTP/1.1".to_owned()]);
  Ok(())
}

/// Connect to a fake server that upgrades with the given accept header and
/// assert the streamer refuses the connection after the server saw the request.
async fn assert_handshake_refused(accept: Option<&'static str>) -> FixtureResult<()> {
  let pki = Pki::new()?;
  let dir = tempfile::tempdir()?;
  let (port, requests) =
    fake_upgrade_server(pki.server_config(&["target.test"], Leaf::Valid)?, accept).await?;
  let proxy = RecordingProxy::start(None).await?;
  let cfg = NetworkConfig {
    proxy: Some(proxy.url(None)),
    no_proxy: Vec::new(),
    ca_bundle: Some(pki.write_bundle(dir.path())?),
  };
  let url = format!("wss://target.test:{port}/feed");
  let connected = LiveLogStreamer::connect(&live_log_client(&cfg)?, &url, "t").await;
  assert!(
    connected.is_none(),
    "accept header {accept:?} must fail the handshake"
  );
  let seen = requests.lock().map_err(|e| e.to_string())?.clone();
  assert_eq!(seen, vec!["GET /feed HTTP/1.1".to_owned()]);
  Ok(())
}

#[tokio::test]
async fn live_log_rejects_a_wrong_sec_websocket_accept() -> FixtureResult<()> {
  assert_handshake_refused(Some("c3VyZWx5LW5vdC10aGUtcmlnaHQtb25l")).await
}

#[tokio::test]
async fn live_log_rejects_a_missing_sec_websocket_accept() -> FixtureResult<()> {
  assert_handshake_refused(None).await
}

#[test]
fn live_log_client_fails_closed_on_a_missing_ca_bundle() {
  let missing = std::path::PathBuf::from("/nonexistent/toolu-ca.pem");
  let cfg = NetworkConfig {
    proxy: None,
    no_proxy: Vec::new(),
    ca_bundle: Some(missing.clone()),
  };
  let msg = match live_log_client(&cfg) {
    Err(shared::RunnerError::Config(msg)) => msg,
    Err(other) => format!("expected a Config error, got {other:?}"),
    Ok(_) => "expected an error, got a client".to_owned(),
  };
  assert!(
    msg.starts_with(&format!(
      "[network] ca_bundle {}: cannot read",
      missing.display()
    )),
    "{msg}"
  );
}
