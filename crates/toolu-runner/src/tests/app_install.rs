//! #202 S2 / AC-4: the installation wait, driven in-process against a
//! loopback peer replaying GitHub's documented `GET /app/installations`
//! example (account rewritten to `acme`), with real input lines written into
//! a pipe only once the prompt they answer has been printed. A shell-out test
//! cannot hold a terminal, so the interactive path is exercised here; the
//! unattended path also runs through the real binary in `create_app_test`.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use config::app_store::StoredApp;
use serde_json::Value;
use shared::SecretMasker;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use super::{WaitIo, WaitOutcome, WaitTarget, WaitTiming, installed_line, wait_for_installation};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const PRIVATE_PEM: &str = include_str!("../../../protocol/tests/fixtures/app_jwt_test_key.pem");
const INSTALLATIONS_200: &str =
  include_str!("../../tests/fixtures/github_app/installations_200.json");
const REPOSITORIES_200: &str =
  include_str!("../../tests/fixtures/github_app/installation_repositories_200.json");
const ACCESS_TOKENS_201: &str =
  include_str!("../../tests/fixtures/github_app/access_tokens_201.json");
const URL: &str =
  "https://github.com/apps/toolu-runner-acme/installations/new/permissions?target_id=7";
const PROMPT: &str = "[k] Keep waiting  [o] Open the link again  [c] Cancel";
const TIMING: WaitTiming = WaitTiming {
  timeout: Duration::from_millis(150),
  poll: Duration::from_millis(30),
};

fn app() -> StoredApp {
  StoredApp {
    host: "github.com".to_owned(),
    app_id: 424_242,
    slug: "toolu-runner-acme".to_owned(),
    owner: "acme".to_owned(),
    client_id: "Iv1.toolutest0000000".to_owned(),
    client_secret: String::new(),
    webhook_secret: None,
    pem: PRIVATE_PEM.to_owned(),
    html_url: "https://github.com/apps/toolu-runner-acme".to_owned(),
    created_at: "2026-10-24T00:00:00Z".to_owned(),
    owner_id: Some(7),
    installation_id: None,
  }
}

/// A capture fixture's `response.body`, parsed.
fn body(capture: &str) -> Result<Value, Box<dyn std::error::Error>> {
  let value: Value = serde_json::from_str(capture)?;
  let text = value
    .pointer("/response/body")
    .and_then(Value::as_str)
    .ok_or("capture has no response.body")?;
  Ok(serde_json::from_str(text)?)
}

/// The documented installation list with its one item moved to `acme`
/// (`repository_selection` as documented: `selected`).
fn acme_installations() -> Result<Value, Box<dyn std::error::Error>> {
  let mut list = body(INSTALLATIONS_200)?;
  *list
    .pointer_mut("/0/account/login")
    .ok_or("no account in the fixture")? = Value::from("acme");
  Ok(list)
}

/// `/app/installations`: empty until `installed` is set, then the list.
struct InstallSwitch {
  installed: Arc<AtomicBool>,
  list: Value,
}

impl Respond for InstallSwitch {
  fn respond(&self, _request: &Request) -> ResponseTemplate {
    if self.installed.load(Ordering::SeqCst) {
      ResponseTemplate::new(200).set_body_json(&self.list)
    } else {
      ResponseTemplate::new(200).set_body_json(Value::Array(Vec::new()))
    }
  }
}

/// A peer whose installation appears once the returned flag is set.
async fn peer() -> Result<(MockServer, Arc<AtomicBool>), Box<dyn std::error::Error>> {
  let server = MockServer::start().await;
  let installed = Arc::new(AtomicBool::new(false));
  Mock::given(method("GET"))
    .and(path("/app/installations"))
    .respond_with(InstallSwitch {
      installed: Arc::clone(&installed),
      list: acme_installations()?,
    })
    .mount(&server)
    .await;
  Ok((server, installed))
}

/// Output the wait writes and the test reads while it runs.
#[derive(Clone, Default)]
struct Screen(Arc<Mutex<Vec<u8>>>);

impl Screen {
  fn text(&self) -> String {
    String::from_utf8_lossy(&self.0.lock().expect("screen lock")).into_owned()
  }

  /// Wait (≤ 5 s) until `needle` has been printed `times` times.
  async fn shows(&self, needle: &str, times: usize) -> TestResult {
    for _ in 0..500 {
      if self.text().matches(needle).count() >= times {
        return Ok(());
      }
      tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Err(format!("never printed {needle:?} x{times}: {}", self.text()).into())
  }
}

impl Write for Screen {
  fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
    self.0.lock().expect("screen lock").extend_from_slice(buf);
    Ok(buf.len())
  }

  fn flush(&mut self) -> std::io::Result<()> {
    Ok(())
  }
}

/// A terminal-like input: the test types into the returned writer.
fn keyboard() -> (DuplexStream, Lines<BufReader<DuplexStream>>) {
  let (typing, reading) = tokio::io::duplex(256);
  (typing, BufReader::new(reading).lines())
}

#[tokio::test]
async fn keep_waiting_at_the_timeout_then_the_installation_is_found() -> TestResult {
  let (server, installed) = peer().await?;
  let (client, app, masker) = (
    reqwest::Client::new(),
    app(),
    Mutex::new(SecretMasker::new()),
  );
  let base = server.uri();
  let target = target(&client, &base, &app, &masker);
  let (mut typing, lines) = keyboard();
  let screen = Screen::default();
  let mut out = screen.clone();
  let opened = Mutex::new(Vec::<String>::new());
  let open = |url: &str| opened.lock().expect("lock").push(url.to_owned());
  let io = WaitIo {
    url: URL,
    open: &open,
    input: Some(lines),
    out: &mut out,
  };
  let driver = async {
    screen.shows(PROMPT, 1).await?;
    assert!(
      screen
        .text()
        .contains("The App isn't installed on acme yet.")
    );
    installed.store(true, Ordering::SeqCst);
    typing.write_all(b"k\n").await?;
    TestResult::Ok(())
  };
  let (outcome, driver_result) = tokio::join!(wait_for_installation(&target, TIMING, io), driver);
  driver_result?;
  let WaitOutcome::Installed(installation) = outcome? else {
    return Err(format!("expected Installed: {}", screen.text()).into());
  };
  assert_eq!(installation.id, 1);
  assert!(
    opened.lock().expect("lock").is_empty(),
    "k must not open the link"
  );
  Ok(())
}

#[tokio::test]
async fn open_again_reopens_the_link_and_cancel_ends_the_wait() -> TestResult {
  let (server, _never) = peer().await?;
  let (client, app, masker) = (
    reqwest::Client::new(),
    app(),
    Mutex::new(SecretMasker::new()),
  );
  let base = server.uri();
  let target = target(&client, &base, &app, &masker);
  let (mut typing, lines) = keyboard();
  let screen = Screen::default();
  let mut out = screen.clone();
  let opened = Mutex::new(Vec::<String>::new());
  let open = |url: &str| opened.lock().expect("lock").push(url.to_owned());
  let io = WaitIo {
    url: URL,
    open: &open,
    input: Some(lines),
    out: &mut out,
  };
  let driver = async {
    screen.shows(PROMPT, 1).await?;
    typing.write_all(b"o\n").await?;
    screen.shows(PROMPT, 2).await?;
    typing.write_all(b"c\n").await?;
    TestResult::Ok(())
  };
  let (outcome, driver_result) = tokio::join!(wait_for_installation(&target, TIMING, io), driver);
  driver_result?;
  assert!(
    matches!(outcome?, WaitOutcome::Cancelled),
    "{}",
    screen.text()
  );
  assert_eq!(*opened.lock().expect("lock"), vec![URL.to_owned()]);
  assert!(screen.text().contains(&format!("Opened {URL}")));
  Ok(())
}

#[tokio::test]
async fn typing_c_while_waiting_copies_the_link_over_osc_52() -> TestResult {
  let (server, installed) = peer().await?;
  let (client, app, masker) = (
    reqwest::Client::new(),
    app(),
    Mutex::new(SecretMasker::new()),
  );
  let base = server.uri();
  let target = target(&client, &base, &app, &masker);
  let (mut typing, lines) = keyboard();
  let screen = Screen::default();
  let mut out = screen.clone();
  let io = WaitIo {
    url: URL,
    open: &|_: &str| {},
    input: Some(lines),
    out: &mut out,
  };
  let slow = WaitTiming {
    timeout: Duration::from_secs(30),
    ..TIMING
  };
  let driver = async {
    screen.shows("Waiting for the installation", 1).await?;
    typing.write_all(b"c\n").await?;
    screen.shows("Copied the link", 1).await?;
    installed.store(true, Ordering::SeqCst);
    TestResult::Ok(())
  };
  let (outcome, driver_result) = tokio::join!(wait_for_installation(&target, slow, io), driver);
  driver_result?;
  assert!(matches!(outcome?, WaitOutcome::Installed(_)));
  let text = screen.text();
  // ESC ] 52 ; c ; base64(link) BEL — the bytes a terminal acts on, built
  // here independently of the code under test.
  let osc52 = format!(
    "\u{1b}]52;c;{}\u{7}",
    base64::engine::general_purpose::STANDARD.encode(URL)
  );
  assert!(text.contains(&osc52), "{text:?}");
  Ok(())
}

#[tokio::test]
async fn unattended_gives_up_at_the_timeout() -> TestResult {
  let (server, _never) = peer().await?;
  let (client, app, masker) = (
    reqwest::Client::new(),
    app(),
    Mutex::new(SecretMasker::new()),
  );
  let base = server.uri();
  let target = target(&client, &base, &app, &masker);
  let mut out = Screen::default();
  let io: WaitIo<'_, BufReader<DuplexStream>, _> = WaitIo {
    url: URL,
    open: &|_: &str| {},
    input: None,
    out: &mut out,
  };
  let outcome = wait_for_installation(&target, TIMING, io).await?;
  assert!(matches!(outcome, WaitOutcome::TimedOut));
  assert!(!out.text().contains(PROMPT), "unattended must not prompt");
  Ok(())
}

#[tokio::test]
async fn closed_input_cancels_at_the_timeout() -> TestResult {
  let (server, _never) = peer().await?;
  let (client, app, masker) = (
    reqwest::Client::new(),
    app(),
    Mutex::new(SecretMasker::new()),
  );
  let base = server.uri();
  let target = target(&client, &base, &app, &masker);
  let (typing, lines) = keyboard();
  drop(typing);
  let mut out = Screen::default();
  let io = WaitIo {
    url: URL,
    open: &|_: &str| {},
    input: Some(lines),
    out: &mut out,
  };
  let outcome = wait_for_installation(&target, TIMING, io).await?;
  assert!(matches!(outcome, WaitOutcome::Cancelled));
  Ok(())
}

#[tokio::test]
async fn a_transient_failure_warns_and_keeps_waiting() -> TestResult {
  let server = MockServer::start().await;
  Mock::given(method("GET"))
    .and(path("/app/installations"))
    .respond_with(ResponseTemplate::new(503).set_body_string("unicorn"))
    .up_to_n_times(1)
    .mount(&server)
    .await;
  Mock::given(method("GET"))
    .and(path("/app/installations"))
    .respond_with(ResponseTemplate::new(200).set_body_json(acme_installations()?))
    .mount(&server)
    .await;
  let (client, app, masker) = (
    reqwest::Client::new(),
    app(),
    Mutex::new(SecretMasker::new()),
  );
  let base = server.uri();
  let target = target(&client, &base, &app, &masker);
  let mut out = Screen::default();
  let io: WaitIo<'_, BufReader<DuplexStream>, _> = WaitIo {
    url: URL,
    open: &|_: &str| {},
    input: None,
    out: &mut out,
  };
  let outcome = wait_for_installation(&target, TIMING, io).await?;
  assert!(matches!(outcome, WaitOutcome::Installed(_)));
  let text = out.text();
  assert!(
    text.contains(
      "warning: GitHub App installation list failed with status 503 Service Unavailable \
       (transient — retried after a pause): unicorn; still waiting"
    ),
    "{text}"
  );
  Ok(())
}

#[tokio::test]
async fn a_rejected_app_ends_the_wait_with_its_error() -> TestResult {
  let server = MockServer::start().await;
  Mock::given(method("GET"))
    .and(path("/app/installations"))
    .respond_with(
      ResponseTemplate::new(401).set_body_string("A JSON web token could not be decoded"),
    )
    .mount(&server)
    .await;
  let (client, app, masker) = (
    reqwest::Client::new(),
    app(),
    Mutex::new(SecretMasker::new()),
  );
  let base = server.uri();
  let target = target(&client, &base, &app, &masker);
  let mut out = Screen::default();
  let io: WaitIo<'_, BufReader<DuplexStream>, _> = WaitIo {
    url: URL,
    open: &|_: &str| {},
    input: None,
    out: &mut out,
  };
  let err = wait_for_installation(&target, TIMING, io)
    .await
    .err()
    .ok_or("a 401 must end the wait")?;
  let shared::RunnerError::Auth(msg) = err else {
    return Err(format!("expected Auth, got {err:?}").into());
  };
  assert!(
    msg.contains("status 401") && msg.contains("A JSON web token could not be decoded"),
    "{msg}"
  );
  Ok(())
}

#[tokio::test]
async fn the_installed_line_counts_selected_repositories_or_says_all() -> TestResult {
  let server = MockServer::start().await;
  Mock::given(method("POST"))
    .and(path("/app/installations/1/access_tokens"))
    .respond_with(ResponseTemplate::new(201).set_body_json(body(ACCESS_TOKENS_201)?))
    .mount(&server)
    .await;
  Mock::given(method("GET"))
    .and(path("/installation/repositories"))
    .respond_with(ResponseTemplate::new(200).set_body_json(body(REPOSITORIES_200)?))
    .mount(&server)
    .await;
  let (client, app, masker) = (
    reqwest::Client::new(),
    app(),
    Mutex::new(SecretMasker::new()),
  );
  let base = server.uri();
  let target = target(&client, &base, &app, &masker);
  let list: Vec<protocol::app_jwt::Installation> = serde_json::from_value(acme_installations()?)?;
  let mut installation = list.into_iter().next().ok_or("one installation")?;
  assert_eq!(
    installed_line(&target, &installation).await,
    "Installed \u{b7} installation 1 \u{b7} 1 selected repository"
  );
  installation.repository_selection = Some("all".to_owned());
  assert_eq!(
    installed_line(&target, &installation).await,
    "Installed \u{b7} installation 1 \u{b7} all repositories"
  );
  Ok(())
}

fn target<'a>(
  client: &'a reqwest::Client,
  api_base: &'a str,
  app: &'a StoredApp,
  masker: &'a Mutex<SecretMasker>,
) -> WaitTarget<'a> {
  WaitTarget {
    client,
    api_base,
    app,
    account: "acme",
    masker,
  }
}

#[tokio::test]
async fn typed_lines_do_not_trigger_extra_polls() -> TestResult {
  let (server, installed) = peer().await?;
  let (client, app, masker) = (
    reqwest::Client::new(),
    app(),
    Mutex::new(SecretMasker::new()),
  );
  let base = server.uri();
  let target = target(&client, &base, &app, &masker);
  let (mut typing, lines) = keyboard();
  let screen = Screen::default();
  let mut out = screen.clone();
  let io = WaitIo {
    url: URL,
    open: &|_: &str| {},
    input: Some(lines),
    out: &mut out,
  };
  let timing = WaitTiming {
    timeout: Duration::from_secs(30),
    poll: Duration::from_millis(800),
  };
  let driver = async {
    screen.shows("Waiting for the installation", 1).await?;
    for typed in 1..=5 {
      typing.write_all(b"?\n").await?;
      screen.shows("Still waiting.", typed).await?;
    }
    let polls = server
      .received_requests()
      .await
      .ok_or("recording off")?
      .len();
    assert_eq!(polls, 1, "five typed lines must not add API calls");
    installed.store(true, Ordering::SeqCst);
    TestResult::Ok(())
  };
  let (outcome, driver_result) = tokio::join!(wait_for_installation(&target, timing, io), driver);
  driver_result?;
  assert!(matches!(outcome?, WaitOutcome::Installed(_)));
  Ok(())
}

#[tokio::test]
async fn the_installed_line_pluralizes_and_falls_back_without_a_count() -> TestResult {
  let server = MockServer::start().await;
  let mut two = body(REPOSITORIES_200)?;
  *two.pointer_mut("/total_count").ok_or("no total_count")? = Value::from(2);
  Mock::given(method("POST"))
    .and(path("/app/installations/1/access_tokens"))
    .respond_with(ResponseTemplate::new(201).set_body_json(body(ACCESS_TOKENS_201)?))
    .mount(&server)
    .await;
  Mock::given(method("GET"))
    .and(path("/installation/repositories"))
    .respond_with(ResponseTemplate::new(200).set_body_json(two))
    .mount(&server)
    .await;
  let (client, app, masker) = (
    reqwest::Client::new(),
    app(),
    Mutex::new(SecretMasker::new()),
  );
  let base = server.uri();
  let list: Vec<protocol::app_jwt::Installation> = serde_json::from_value(acme_installations()?)?;
  let installation = list.into_iter().next().ok_or("one installation")?;
  assert_eq!(
    installed_line(&target(&client, &base, &app, &masker), &installation).await,
    "Installed \u{b7} installation 1 \u{b7} 2 selected repositories"
  );
  // A peer that cannot mint a token: the count is left out, never fatal.
  let empty = MockServer::start().await;
  let empty_base = empty.uri();
  assert_eq!(
    installed_line(&target(&client, &empty_base, &app, &masker), &installation).await,
    "Installed \u{b7} installation 1 \u{b7} selected repositories"
  );
  Ok(())
}
