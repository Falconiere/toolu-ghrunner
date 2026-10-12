//! #94 AC-8: the live-log WebSocket rides the shared `network` client — a
//! real `wss://` server behind a real authenticated CONNECT proxy, trusted
//! only through the generated `ca_bundle`.

#[path = "../../network/tests/helpers/network_fixtures.rs"]
mod network_fixtures;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use network_fixtures::{FixtureResult, Leaf, Pki, RecordingProxy, TlsTarget};
use shared::NetworkConfig;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::ServerConfig;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use wire::reporting::live_log::{LiveLogLine, LiveLogStreamer};

/// What a TLS WebSocket server saw: the `Authorization` header and every
/// text frame.
#[derive(Default)]
struct Seen {
  auth: Option<String>,
  frames: Vec<String>,
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
      let header_sink = Arc::clone(&sink);
      let callback = move |request: &Request, response: Response| {
        if let Ok(mut s) = header_sink.lock() {
          s.auth = request
            .headers()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        }
        Ok(response)
      };
      let Ok(mut ws) = tokio_tungstenite::accept_hdr_async(tls, callback).await else {
        continue;
      };
      use futures_util::StreamExt as _;
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

fn live_client(cfg: &NetworkConfig) -> FixtureResult<reqwest::Client> {
  Ok(network::client_builder(cfg)?.http1_only().build()?)
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
  let client = live_client(&cfg)?;
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
  let hits = proxy.hits();
  assert_eq!(hits.len(), 1, "{hits:?}");
  assert_eq!(hits[0].line, format!("CONNECT target.test:{port} HTTP/1.1"));
  let s = seen.lock().map_err(|_| "poisoned")?;
  assert_eq!(s.auth.as_deref(), Some("Bearer feed-token"));
  assert!(
    s.frames.iter().any(|f| f.contains("hello through the proxy")),
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
  let connected = LiveLogStreamer::connect(&live_client(&cfg)?, &url, "t").await;
  assert!(connected.is_none(), "TLS to an untrusted CA must fail closed");
  assert!(seen.lock().map(|s| s.frames.is_empty()).unwrap_or(false));
  Ok(())
}

#[tokio::test]
async fn live_log_rejects_a_server_that_does_not_upgrade() -> FixtureResult<()> {
  let pki = Pki::new()?;
  let dir = tempfile::tempdir()?;
  let plain = TlsTarget::ok(pki.server_config(&["target.test"], Leaf::Valid)?).await?;
  let proxy = RecordingProxy::start(None).await?;
  let cfg = NetworkConfig {
    proxy: Some(proxy.url(None)),
    no_proxy: Vec::new(),
    ca_bundle: Some(pki.write_bundle(dir.path())?),
  };
  let url = format!("wss://target.test:{}/feed", plain.port());
  let connected = LiveLogStreamer::connect(&live_client(&cfg)?, &url, "t").await;
  assert!(connected.is_none(), "a 200 answer is not a WebSocket");
  assert_eq!(plain.requests(), vec!["GET /feed HTTP/1.1".to_owned()]);
  Ok(())
}
