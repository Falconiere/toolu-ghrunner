//! Tests for the `doctor` GitHub auth rows: API-base derivation (the same
//! rule `register` uses), host grouping across real loaded registrations, the
//! verdict on the captured real `GET /user` 200 and 401 bodies, and the
//! credentialed request itself against a real loopback socket.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::time::{Duration, Instant};

use config::config::{
  CacheSection, RunnerRegistrationConfig, RuntimeConfig, ServicesSection, ShadowSection,
  WorkspaceSection, load_config, save_config,
};
use shared::NetworkConfig;

use super::{AuthHost, api_base, auth_hosts, verdict_row, verify};
use crate::doctor_report::{Level, Row};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const USER_200: &str = include_str!("../../tests/fixtures/doctor/github_user_200.json");
const USER_401: &str = include_str!("../../tests/fixtures/doctor/github_user_401.json");
const GHES_JIT: &str = include_str!("../../tests/fixtures/jit_config_ghes.json");
const STORED: &str = "stored login (0600 file)";
const BEARER: &str = "unit-test-bearer";

/// Persist a registration at `runner_url` under `<home>/<name>/` and load it
/// back the way `run` sees it.
fn loaded(
  home: &Path,
  name: &str,
  runner_url: &str,
  ephemeral: bool,
  network: config::network::NetworkSection,
) -> Result<RunnerRegistrationConfig, Box<dyn std::error::Error>> {
  let fixture: serde_json::Value = serde_json::from_str(GHES_JIT)?;
  let jit_config = fixture
    .get("encoded")
    .and_then(serde_json::Value::as_str)
    .ok_or("fixture has no encoded field")?
    .to_owned();
  let dir = home.join(name);
  let config_path = dir.join("config.toml");
  save_config(
    &config_path,
    &RunnerRegistrationConfig {
      runner_url: runner_url.to_owned(),
      runner_name: name.to_owned(),
      runner_id: 7,
      auth_token: "client-id".to_owned(),
      labels: vec!["self-hosted".to_owned()],
      runner_group: "Default".to_owned(),
      ephemeral,
      runtime: RuntimeConfig {
        jit_config,
        work_dir: dir.join("_work").to_string_lossy().into_owned(),
        data_dir: dir.to_string_lossy().into_owned(),
        protocol_version: "v1".to_owned(),
      },
      services: ServicesSection::default(),
      cache: CacheSection::default(),
      workspace: WorkspaceSection::default(),
      shadow: ShadowSection::default(),
      network,
    },
  )?;
  Ok(load_config(&config_path)?)
}

fn no_network() -> config::network::NetworkSection {
  config::network::NetworkSection::default()
}

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
  let listener = TcpListener::bind("127.0.0.1:0")?;
  listener.set_nonblocking(true)?;
  let base = format!("http://{}", listener.local_addr()?);
  let extra = headers
    .iter()
    .map(|(name, value)| format!("{name}: {value}\r\n"))
    .collect::<Vec<_>>()
    .concat();
  let response = format!(
    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
    body.len()
  );
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

#[test]
fn api_base_follows_registers_rule() {
  assert_eq!(
    api_base("https://github.com/o/r"),
    Some(("github.com".to_owned(), "https://api.github.com".to_owned()))
  );
  assert_eq!(
    api_base("https://ghes.example.com/o/r"),
    Some((
      "ghes.example.com".to_owned(),
      "https://ghes.example.com/api/v3".to_owned()
    ))
  );
  assert_eq!(
    api_base("http://127.0.0.1:8080/o/r"),
    Some((
      "127.0.0.1".to_owned(),
      "http://127.0.0.1:8080/api/v3".to_owned()
    ))
  );
  assert_eq!(api_base("not a url"), None);
  assert_eq!(api_base("file:///tmp/o/r"), None);
  assert_eq!(api_base(""), None);
}

#[test]
fn api_base_agrees_with_the_one_register_posts_to() -> TestResult {
  for (runner_url, host, expected) in [
    (
      "https://github.com/o/r",
      "github.com",
      "https://api.github.com",
    ),
    (
      "https://GitHub.com/o/r",
      "github.com",
      "https://api.github.com",
    ),
    (
      "https://ghes.example.com/o/r",
      "ghes.example.com",
      "https://ghes.example.com/api/v3",
    ),
    (
      "https://ghes.example.com:8443/o/r",
      "ghes.example.com",
      "https://ghes.example.com:8443/api/v3",
    ),
    (
      "http://ghes.example.com:8080/o/r",
      "ghes.example.com",
      "http://ghes.example.com:8080/api/v3",
    ),
  ] {
    let parsed = url::Url::parse(runner_url)?;
    let register_base = wire::net::register::api_base(&parsed, host);
    assert_eq!(register_base, expected, "{runner_url}");
    assert_eq!(
      api_base(runner_url),
      Some((host.to_owned(), register_base)),
      "{runner_url}"
    );
  }
  Ok(())
}

#[test]
fn registrations_on_one_host_need_a_bearer_when_any_is_persistent() -> TestResult {
  let home = tempfile::tempdir()?;
  let persistent = loaded(
    home.path(),
    "persistent",
    "https://ghes.example.com/o/persistent",
    false,
    no_network(),
  )?;
  let other_persistent = loaded(
    home.path(),
    "other-persistent",
    "https://ghes.example.com/o/other",
    false,
    no_network(),
  )?;
  let ephemeral = loaded(
    home.path(),
    "ephemeral",
    "https://ghes.example.com/o/ephemeral",
    true,
    no_network(),
  )?;
  let other_ephemeral = loaded(
    home.path(),
    "other-ephemeral",
    "https://ghes.example.com/o/other-e",
    true,
    no_network(),
  )?;

  for (configs, needs_bearer) in [
    (vec![&ephemeral, &persistent], true),
    (vec![&persistent, &ephemeral], true),
    (vec![&persistent, &other_persistent], true),
    (vec![&ephemeral, &other_ephemeral], false),
    (vec![&ephemeral], false),
    (vec![&persistent], true),
  ] {
    let hosts = auth_hosts(&configs);
    assert_eq!(hosts.len(), 1);
    let host = hosts.first().ok_or("no host")?;
    assert_eq!(host.host, "ghes.example.com");
    assert_eq!(host.api_base, "https://ghes.example.com/api/v3");
    assert_eq!(
      host.needs_bearer,
      needs_bearer,
      "{:?}",
      configs.iter().map(|c| c.ephemeral).collect::<Vec<_>>()
    );
  }
  Ok(())
}

#[test]
fn distinct_hosts_and_api_bases_stay_separate_in_registration_order() -> TestResult {
  let home = tempfile::tempdir()?;
  let dotcom = loaded(
    home.path(),
    "dotcom",
    "https://github.com/o/r",
    false,
    no_network(),
  )?;
  let ghes = loaded(
    home.path(),
    "ghes",
    "https://ghes.example.com/o/r",
    true,
    no_network(),
  )?;
  let port_a = loaded(
    home.path(),
    "port-a",
    "http://127.0.0.1:8080/o/r",
    false,
    no_network(),
  )?;
  let port_b = loaded(
    home.path(),
    "port-b",
    "http://127.0.0.1:9090/o/r",
    true,
    no_network(),
  )?;

  let hosts = auth_hosts(&[&dotcom, &ghes, &port_a, &port_b]);
  let seen: Vec<(&str, &str, bool)> = hosts
    .iter()
    .map(|host| {
      (
        host.host.as_str(),
        host.api_base.as_str(),
        host.needs_bearer,
      )
    })
    .collect();
  assert_eq!(
    seen,
    vec![
      ("github.com", "https://api.github.com", true),
      ("ghes.example.com", "https://ghes.example.com/api/v3", false),
      ("127.0.0.1", "http://127.0.0.1:8080/api/v3", true),
      ("127.0.0.1", "http://127.0.0.1:9090/api/v3", false),
    ]
  );
  Ok(())
}

#[test]
fn without_a_usable_registration_the_check_falls_back_to_github_com() -> TestResult {
  let home = tempfile::tempdir()?;
  let unparseable = loaded(home.path(), "unparseable", "not a url", false, no_network())?;
  for configs in [Vec::new(), vec![&unparseable]] {
    let hosts = auth_hosts(&configs);
    assert_eq!(hosts.len(), 1);
    let host = hosts.first().ok_or("no host")?;
    assert_eq!(host.host, "github.com");
    assert_eq!(host.api_base, "https://api.github.com");
    assert!(!host.needs_bearer);
    assert_eq!(host.network, NetworkConfig::default());
  }
  Ok(())
}

#[test]
fn a_host_uses_the_network_settings_of_its_first_registration() -> TestResult {
  let home = tempfile::tempdir()?;
  let proxied = config::network::NetworkSection {
    proxy: Some("http://proxy.example:8080".to_owned()),
    no_proxy: vec!["internal.example".to_owned()],
    ca_bundle: None,
  };
  let with_proxy = loaded(
    home.path(),
    "with-proxy",
    "https://ghes.example.com/o/a",
    false,
    proxied,
  )?;
  let plain = loaded(
    home.path(),
    "plain",
    "https://ghes.example.com/o/b",
    false,
    no_network(),
  )?;
  let expected = NetworkConfig {
    proxy: Some("http://proxy.example:8080".to_owned()),
    no_proxy: vec!["internal.example".to_owned()],
    ca_bundle: None,
  };
  assert_eq!(with_proxy.network_config(), expected);

  let proxied_first = auth_hosts(&[&with_proxy, &plain]);
  assert_eq!(proxied_first.first().ok_or("no host")?.network, expected);
  let plain_first = auth_hosts(&[&plain, &with_proxy]);
  assert_eq!(
    plain_first.first().ok_or("no host")?.network,
    NetworkConfig::default()
  );
  Ok(())
}

#[test]
fn a_real_200_names_the_login_and_scopes() {
  assert_eq!(
    verdict_row(
      "TOOLU_RUNNER_TOKEN",
      200,
      USER_200,
      Some("repo, admin:org"),
      "github.com",
      true,
    ),
    Row::new(
      Level::Ok,
      "token",
      "TOOLU_RUNNER_TOKEN: valid (login Falconiere; scopes repo, admin:org)"
    )
  );
  for scopes in [None, Some("")] {
    assert_eq!(
      verdict_row(STORED, 200, USER_200, scopes, "github.com", true),
      Row::new(
        Level::Ok,
        "token",
        "stored login (0600 file): valid (login Falconiere)"
      ),
      "{scopes:?}"
    );
  }
  // A 200 whose body is not the user document still verifies the token.
  assert_eq!(
    verdict_row(STORED, 200, "", None, "github.com", true),
    Row::new(
      Level::Ok,
      "token",
      "stored login (0600 file): valid (login unknown)"
    )
  );
}

#[test]
fn a_real_401_fails_with_login_and_other_statuses_warn() {
  assert_eq!(
    verdict_row(STORED, 401, USER_401, None, "github.com", true),
    Row::new(
      Level::Fail,
      "token",
      "stored login (0600 file): rejected by GitHub (HTTP 401: Bad credentials)"
    )
    .with_fix("toolu-runner login")
  );
  assert_eq!(
    verdict_row(STORED, 401, USER_401, None, "ghes.example.com", true),
    Row::new(
      Level::Fail,
      "token",
      "stored login (0600 file): rejected by GitHub (HTTP 401: Bad credentials)"
    )
    .with_fix("toolu-runner login ghes.example.com")
  );
  // No parseable reason in the body: the status stands alone.
  assert_eq!(
    verdict_row(STORED, 401, "", None, "github.com", true).detail,
    "stored login (0600 file): rejected by GitHub (HTTP 401)"
  );

  for status in [403, 500, 502] {
    for needs_bearer in [true, false] {
      assert_eq!(
        verdict_row(
          "TOOLU_RUNNER_TOKEN",
          status,
          "",
          None,
          "github.com",
          needs_bearer
        ),
        Row::new(
          Level::Warn,
          "token",
          format!("TOOLU_RUNNER_TOKEN: could not verify (HTTP {status})")
        ),
        "{status} needs_bearer={needs_bearer}"
      );
    }
  }
}

#[test]
fn a_rejected_token_only_fails_when_a_persistent_registration_needs_it() {
  assert_eq!(
    verdict_row(STORED, 401, USER_401, None, "github.com", false),
    Row::new(
      Level::Warn,
      "token",
      "stored login (0600 file): rejected by GitHub (HTTP 401: Bad credentials)"
    )
    .with_fix("toolu-runner login")
  );
}

#[test]
fn a_rejected_env_token_is_fixed_by_replacing_it_not_by_login_alone() {
  assert_eq!(
    verdict_row(
      "TOOLU_RUNNER_TOKEN",
      401,
      USER_401,
      None,
      "github.com",
      true
    ),
    Row::new(
      Level::Fail,
      "token",
      "TOOLU_RUNNER_TOKEN: rejected by GitHub (HTTP 401: Bad credentials)"
    )
    .with_fix("export a valid TOOLU_RUNNER_TOKEN, or unset it and run `toolu-runner login`")
  );
  assert_eq!(
    verdict_row(
      "TOOLU_RUNNER_TOKEN",
      401,
      USER_401,
      None,
      "ghes.example.com",
      false
    ),
    Row::new(
      Level::Warn,
      "token",
      "TOOLU_RUNNER_TOKEN: rejected by GitHub (HTTP 401: Bad credentials)"
    )
    .with_fix(
      "export a valid TOOLU_RUNNER_TOKEN, or unset it and run `toolu-runner login ghes.example.com`"
    )
  );
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
