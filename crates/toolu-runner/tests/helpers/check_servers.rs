//! Shared fixtures for the `check` shell-out tests: a raw-TCP recording
//! HTTP peer (plain target, forward proxy, CONNECT proxy, 407 proxy or a
//! peer that never answers) and an isolated `toolu-runner check` command.
//!
//! The peer is a real socket server, not a mock of the code under test: the
//! binary's real reqwest client, proxy matcher and TLS stack talk to it, and
//! the test asserts on the request heads it actually received.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;

/// The committed GHES JIT registration fixture.
const GHES_JIT: &str = include_str!("../fixtures/jit_config_ghes.json");

/// A `200 OK` with an empty body.
pub const OK: &str = "HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";

/// Decide the reply for a received request head; `None` holds the socket
/// open without ever answering.
pub type Respond = fn(&str) -> Option<&'static str>;

/// A listening recording peer on `127.0.0.1`.
pub struct Recorder {
  addr: SocketAddr,
  heads: Arc<Mutex<Vec<String>>>,
}

impl Recorder {
  /// Bind an ephemeral port and serve every connection on its own thread.
  pub fn spawn(respond: Respond) -> std::io::Result<Self> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let addr = listener.local_addr()?;
    let heads = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&heads);
    std::thread::spawn(move || {
      for stream in listener.incoming().flatten() {
        let sink = Arc::clone(&sink);
        std::thread::spawn(move || serve(stream, &sink, respond));
      }
    });
    Ok(Self { addr, heads })
  }

  /// The bound port.
  pub fn port(&self) -> u16 {
    self.addr.port()
  }

  /// Every request head received so far (request line + headers).
  /// A poisoned lock still yields what was recorded, never an empty list.
  pub fn heads(&self) -> Vec<String> {
    match self.heads.lock() {
      Ok(heads) => heads.clone(),
      Err(poisoned) => poisoned.into_inner().clone(),
    }
  }

  /// Just the request lines (`GET /path HTTP/1.1`).
  pub fn request_lines(&self) -> Vec<String> {
    self
      .heads()
      .iter()
      .filter_map(|head| head.lines().next().map(str::to_owned))
      .collect()
  }
}

/// Read a request head (up to the blank line, EOF, or a 2 s lull — a TLS
/// `ClientHello` never contains one), record it, then reply or hold.
fn serve(mut stream: TcpStream, sink: &Mutex<Vec<String>>, respond: Respond) {
  let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
  let mut buf = Vec::new();
  let mut chunk = [0_u8; 4096];
  while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < 65_536 {
    match stream.read(&mut chunk) {
      Ok(0) | Err(_) => break,
      Ok(n) => buf.extend(chunk.iter().take(n)),
    }
  }
  let head = String::from_utf8_lossy(&buf).into_owned();
  if let Ok(mut heads) = sink.lock() {
    heads.push(head.clone());
  }
  match respond(&head) {
    Some(reply) => {
      let _ = stream.write_all(reply.as_bytes());
      let _ = stream.flush();
    },
    None => std::thread::sleep(Duration::from_secs(60)),
  }
}

/// A loopback port with nothing listening (bound, then released).
pub fn closed_port() -> std::io::Result<u16> {
  let listener = TcpListener::bind("127.0.0.1:0")?;
  let port = listener.local_addr()?.port();
  drop(listener);
  Ok(port)
}

/// Every variable that changes the probes' routing or trust.
const ROUTING_VARS: [&str; 13] = [
  "HTTPS_PROXY",
  "https_proxy",
  "HTTP_PROXY",
  "http_proxy",
  "ALL_PROXY",
  "all_proxy",
  "NO_PROXY",
  "no_proxy",
  "REQUEST_METHOD",
  "SSL_CERT_FILE",
  "SSL_CERT_DIR",
  "NODE_EXTRA_CA_CERTS",
  "DOCKER_HOST",
];

/// `toolu-runner check` pinned to an isolated home and cwd. Hermetic by
/// default: `https://` traffic (the optional `node-dist` probe) goes to a
/// closed loopback proxy, and loopback targets bypass it via `NO_PROXY`.
pub fn check_cmd(home: &Path) -> std::io::Result<Command> {
  let mut cmd = Command::new(env!("CARGO_BIN_EXE_toolu-runner"));
  cmd
    .arg("check")
    .current_dir(home)
    .env("HOME", home)
    .env("TOOLU_RUNNER_HOME", home);
  for var in ROUTING_VARS {
    cmd.env_remove(var);
  }
  cmd
    .env(
      "HTTPS_PROXY",
      format!("http://127.0.0.1:{}", closed_port()?),
    )
    .env("NO_PROXY", "127.0.0.1,localhost");
  Ok(cmd)
}

/// Run `cmd`, returning (exit code, stdout, stderr, elapsed).
pub fn run(cmd: &mut Command) -> std::io::Result<(Option<i32>, String, String, Duration)> {
  let started = Instant::now();
  let output = cmd.output()?;
  Ok((
    output.status.code(),
    String::from_utf8_lossy(&output.stdout).into_owned(),
    String::from_utf8_lossy(&output.stderr).into_owned(),
    started.elapsed(),
  ))
}

/// The GHES fixture's JIT envelope with its service URLs pointed at `base`.
pub fn jit_pointing_at(base: &str) -> Result<String, Box<dyn std::error::Error>> {
  let fixture: serde_json::Value = serde_json::from_str(GHES_JIT)?;
  let encoded = fixture
    .get("encoded")
    .and_then(serde_json::Value::as_str)
    .ok_or("fixture has no encoded field")?;
  let mut outer: serde_json::Map<String, serde_json::Value> =
    serde_json::from_slice(&B64.decode(encoded)?)?;
  let mut rewrite =
    |blob: &str, edit: &dyn Fn(&mut serde_json::Value)| -> Result<(), Box<dyn std::error::Error>> {
      let raw = outer
        .get(blob)
        .and_then(serde_json::Value::as_str)
        .ok_or("blob is not a string")?;
      let mut value: serde_json::Value = serde_json::from_slice(&B64.decode(raw)?)?;
      edit(&mut value);
      outer.insert(
        blob.to_owned(),
        B64.encode(serde_json::to_vec(&value)?).into(),
      );
      Ok(())
    };
  // A missing key is a silent no-op here; the recorded request paths in the
  // caller's assertions catch it.
  rewrite(".runner", &|runner| {
    if let Some(runner) = runner.as_object_mut() {
      runner.insert(
        "ServerUrl".to_owned(),
        format!("{base}/pipelines/tenant/").into(),
      );
      runner.insert(
        "ServerUrlV2".to_owned(),
        format!("{base}/broker/tenant/").into(),
      );
    }
  })?;
  rewrite(".credentials", &|creds| {
    if let Some(data) = creds
      .get_mut("Data")
      .and_then(serde_json::Value::as_object_mut)
    {
      data.insert(
        "AuthorizationUrl".to_owned(),
        format!("{base}/token/oauth").into(),
      );
    }
  })?;
  Ok(B64.encode(serde_json::to_vec(&outer)?))
}
