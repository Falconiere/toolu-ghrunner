//! `doctor` auth `verify` against real loopback sockets: the request it
//! sends, the captured `GET /user` 200 and 401 bodies, a body cut short, no
//! answer at all, and an unusable `[network]`. Split from `doctor_auth.rs`.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};

use shared::NetworkConfig;

use super::{AuthHost, verify};
use crate::doctor_report::{Level, Row};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const USER_200: &str = include_str!("../../tests/fixtures/doctor/github_user_200.json");
const USER_401: &str = include_str!("../../tests/fixtures/doctor/github_user_401.json");
const STORED: &str = "stored login (0600 file)";
const BEARER: &str = "unit-test-bearer";

/// Config mode with a proxy nothing listens on: loopback is always bypassed,
/// so requests go direct and the developer's `HTTP(S)_PROXY` never applies.
fn hermetic_network() -> NetworkConfig {
  NetworkConfig {
    proxy: Some("http://127.0.0.1:9".to_owned()),
    no_proxy: Vec::new(),
    ca_bundle: None,
  }
}

/// An auth target whose API lives at `api_base`.
fn target(api_base: &str, needs_bearer: bool) -> AuthHost {
  AuthHost {
    host: "ghes.example.com".to_owned(),
    api_base: api_base.to_owned(),
    needs_bearer,
    network: hermetic_network(),
  }
}

/// A one-shot loopback HTTP peer: the thread that serves the first connection.
struct Peer {
  handle: std::thread::JoinHandle<std::io::Result<String>>,
}

impl Peer {
  /// Wait for the peer; the request head it received.
  fn head(self) -> Result<String, Box<dyn std::error::Error>> {
    match self.handle.join() {
      Ok(head) => Ok(head?),
      Err(_panic) => Err("the loopback peer thread panicked".into()),
    }
  }
}

/// Serve `status`, `headers` and `body` to the first connection on a loopback
/// port. Returns its base URL and the peer.
fn serve_once(
  status: &str,
  headers: &[(&str, &str)],
  body: &str,
) -> Result<(String, Peer), Box<dyn std::error::Error>> {
  let extra = headers
    .iter()
    .map(|(name, value)| format!("{name}: {value}\r\n"))
    .collect::<Vec<_>>()
    .concat();
  serve_raw_once(format!(
    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
    body.len()
  ))
}

/// Write `response` verbatim to the first connection on a loopback port, then
/// close it. Returns its base URL and the peer.
fn serve_raw_once(response: String) -> Result<(String, Peer), Box<dyn std::error::Error>> {
  let listener = TcpListener::bind("127.0.0.1:0")?;
  listener.set_nonblocking(true)?;
  let base = format!("http://{}", listener.local_addr()?);
  let handle = std::thread::spawn(move || {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut stream = loop {
      match listener.accept() {
        Ok((stream, _)) => break stream,
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
          if Instant::now() > deadline {
            return Err(std::io::Error::new(
              std::io::ErrorKind::TimedOut,
              "no client connected within 10 s",
            ));
          }
          std::thread::sleep(Duration::from_millis(5));
        },
        Err(error) => return Err(error),
      }
    };
    stream.set_nonblocking(false)?;
    let mut head = Vec::new();
    let mut chunk = [0_u8; 1024];
    while !head.windows(4).any(|window| window == b"\r\n\r\n") {
      let read = stream.read(&mut chunk)?;
      if read == 0 {
        break;
      }
      head.extend_from_slice(chunk.get(..read).unwrap_or_default());
    }
    stream.write_all(response.as_bytes())?;
    Ok(String::from_utf8_lossy(&head).into_owned())
  });
  Ok((base, Peer { handle }))
}

/// Whether the request head has `line` (case-insensitively).
fn has_line(head: &str, line: &str) -> bool {
  head.lines().any(|got| got.eq_ignore_ascii_case(line))
}

#[tokio::test]
async fn verify_sends_the_bearer_to_user_and_reads_the_real_200() -> TestResult {
  let (base, request) = serve_once("200 OK", &[("X-OAuth-Scopes", "repo, admin:org")], USER_200)?;
  let row = verify(&target(&base, true), STORED, BEARER, Duration::from_secs(5)).await;
  assert_eq!(
    row,
    Row::new(
      Level::Ok,
      "token",
      "stored login (0600 file): valid (login Falconiere; scopes repo, admin:org)"
    )
  );

  let head = request.head()?;
  assert!(has_line(&head, "GET /user HTTP/1.1"), "{head}");
  assert!(
    has_line(&head, &format!("authorization: Bearer {BEARER}")),
    "{head}"
  );
  assert!(
    has_line(&head, "accept: application/vnd.github+json"),
    "{head}"
  );
  Ok(())
}

#[tokio::test]
async fn verify_turns_a_real_401_into_the_rejected_row() -> TestResult {
  let (base, request) = serve_once("401 Unauthorized", &[], USER_401)?;
  let row = verify(&target(&base, true), STORED, BEARER, Duration::from_secs(5)).await;
  assert_eq!(
    row,
    Row::new(
      Level::Fail,
      "token",
      "stored login (0600 file): rejected by GitHub (HTTP 401: Bad credentials)"
    )
    .with_fix("toolu-runner login ghes.example.com")
  );
  request.head()?;
  Ok(())
}

#[tokio::test]
async fn an_unreadable_200_body_warns_instead_of_passing_for_empty() -> TestResult {
  // The real 200 body cut short: the peer promises all of it, sends half and
  // closes, so reading the body fails after the status arrived.
  let half = USER_200
    .get(..USER_200.len() / 2)
    .ok_or("the fixture is not ASCII")?;
  let (base, request) = serve_raw_once(format!(
    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{half}",
    USER_200.len()
  ))?;
  let row = verify(&target(&base, true), STORED, BEARER, Duration::from_secs(5)).await;
  request.head()?;
  assert_eq!(
    row,
    Row::new(
      Level::Warn,
      "token",
      "stored login (0600 file): could not verify: unreadable response (HTTP 200): error decoding response body"
    )
  );
  Ok(())
}

#[tokio::test]
async fn verify_without_an_answer_warns_that_it_could_not_verify() -> TestResult {
  // Nothing listening: bind a port, then close it.
  let closed = TcpListener::bind("127.0.0.1:0")?;
  let addr = closed.local_addr()?;
  drop(closed);
  let refused = std::net::TcpStream::connect(addr)
    .err()
    .ok_or("a closed port accepted a connection")?;

  let base = format!("http://{addr}");
  let row = verify(&target(&base, true), STORED, BEARER, Duration::from_secs(5)).await;
  assert_eq!(
    row,
    Row::new(
      Level::Warn,
      "token",
      format!(
        "stored login (0600 file): could not verify: connect: error sending request for url ({base}/user): client error (Connect): tcp connect error: {refused}"
      )
    )
  );
  assert!(!row.detail.contains(BEARER), "{}", row.detail);
  Ok(())
}

#[tokio::test]
async fn verify_with_an_unusable_network_section_warns_without_a_request() {
  let mut unusable = target("http://127.0.0.1:9", true);
  unusable.network = NetworkConfig {
    proxy: None,
    no_proxy: Vec::new(),
    ca_bundle: Some("relative/ca.pem".into()),
  };
  let row = verify(&unusable, STORED, BEARER, Duration::from_secs(1)).await;
  assert_eq!(
    row,
    Row::new(
      Level::Warn,
      "token",
      "stored login (0600 file): could not verify: config error: [network] ca_bundle relative/ca.pem: must be an absolute path"
    )
  );
}
