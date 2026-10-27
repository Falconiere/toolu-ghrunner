//! `toolu-runner setup` (#204) through the real binary against a real
//! loopback GitHub peer (S1–S10): a stateful responder, not a mock of
//! the code under test — the peer flips to "registered" only when the
//! binary actually POSTs `generate-jitconfig`, and `online` is only
//! reported after that. Real temp HOME, PATH-stub supervisor.

#![cfg(unix)]

#[cfg(target_os = "linux")]
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use crate::install_service_test::{nonroot_shims, shim_env, write_shim};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Committed `generate-jitconfig` 201 body with a genuine 3-blob envelope.
const RESPONSE_FIXTURE: &str = include_str!("fixtures/generate_jitconfig_response.json");
/// A stub supervisor CLI: log argv, succeed.
const RECORD_OK: &str = "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$SHIM_LOG\"\nexit 0\n";
/// The runner name every unattended scenario registers.
const NAME: &str = "test-runner";

/// Peer knobs a scenario can flip.
#[derive(Default)]
struct PeerState {
  /// Scope keys (`orgs/acme`, `repos/octo/web`) that have registered.
  registered: Mutex<Vec<String>>,
  name_taken: AtomicBool,
  fail_api_generate: AtomicBool,
  groups_fail: AtomicBool,
  admin: AtomicBool,
}

impl PeerState {
  fn new() -> Arc<Self> {
    Arc::new(Self {
      registered: Mutex::new(Vec::new()),
      name_taken: AtomicBool::new(false),
      fail_api_generate: AtomicBool::new(false),
      groups_fail: AtomicBool::new(false),
      admin: AtomicBool::new(true),
    })
  }
}

/// The registration scope a runners/generate path addresses:
/// `orgs/<org>` or `repos/<owner>/<repo>` (the `/api/v3` stripped).
fn scope_key(path: &str) -> String {
  let parts: Vec<&str> = path
    .trim_start_matches("/api/v3/")
    .split('/')
    .filter(|part| !part.is_empty())
    .collect();
  let take = if parts.first() == Some(&"repos") {
    3
  } else {
    2
  };
  parts.into_iter().take(take).collect::<Vec<_>>().join("/")
}

/// The stateful GitHub peer: it answers like GitHub's REST API at the
/// `/api/v3` base a loopback `--github-url` derives.
struct Peer(Arc<PeerState>);

impl Respond for Peer {
  fn respond(&self, request: &Request) -> ResponseTemplate {
    let path = request.url.path().to_owned();
    let method = request.method.as_str();
    let state = &self.0;
    let json_reply = |status: u16, body: Value| ResponseTemplate::new(status).set_body_json(body);
    match (method, path.as_str()) {
      ("GET", "/api/v3/orgs/acme") => json_reply(200, json!({"login": "acme", "id": 1})),
      ("GET", "/api/v3/orgs/ghost") => json_reply(404, json!({"message": "Not Found"})),
      ("GET", "/api/v3/user") => json_reply(200, json!({"login": "octo"})),
      ("GET", "/api/v3/orgs/acme/memberships/octo") => json_reply(
        200,
        json!({"state": "active", "role": if state.admin.load(Ordering::SeqCst) { "admin" } else { "member" }}),
      ),
      ("GET", "/api/v3/orgs/acme/actions/runner-groups") => {
        if state.groups_fail.load(Ordering::SeqCst) {
          json_reply(500, json!({"message": "boom"}))
        } else {
          json_reply(
            200,
            json!({"total_count": 2, "runner_groups": [
              {"id": 1, "name": "Default", "default": true, "allows_public_repositories": false},
              {"id": 7, "name": "ci", "default": false, "allows_public_repositories": true}
            ]}),
          )
        }
      },
      ("GET", "/api/v3/user/repos") => json_reply(
        200,
        json!([{"full_name": "octo/web"}, {"full_name": "octo/api"}]),
      ),
      ("GET", p) if p == "/api/v3/repos/octo/web" || p == "/api/v3/repos/octo/api" => json_reply(
        200,
        json!({"permissions": {"admin": state.admin.load(Ordering::SeqCst)}}),
      ),
      ("POST", p) if p.ends_with("/generate-jitconfig") => {
        if p.contains("/repos/octo/api/") && state.fail_api_generate.load(Ordering::SeqCst) {
          return json_reply(500, json!({"message": "boom"}));
        }
        state
          .registered
          .lock()
          .unwrap_or_else(std::sync::PoisonError::into_inner)
          .push(scope_key(p));
        let body: Value = serde_json::from_str(RESPONSE_FIXTURE).unwrap_or(json!({}));
        ResponseTemplate::new(201).set_body_json(body)
      },
      ("GET", p) if p.ends_with("/actions/runners") => {
        let registered_here = state
          .registered
          .lock()
          .unwrap_or_else(std::sync::PoisonError::into_inner)
          .contains(&scope_key(p));
        if registered_here || (state.name_taken.load(Ordering::SeqCst) && p.contains("/orgs/")) {
          json_reply(
            200,
            json!({"total_count": 1, "runners": [{
              "id": 461, "name": NAME, "status": "online", "busy": false,
              "labels": [
                {"id": 1, "name": "self-hosted", "type": "read-only"},
                {"id": 2, "name": "Linux", "type": "read-only"},
                {"id": 3, "name": "X64", "type": "read-only"}
              ]
            }]}),
          )
        } else {
          json_reply(200, json!({"total_count": 0, "runners": []}))
        }
      },
      ("DELETE", p) if p.contains("/actions/runners/") => ResponseTemplate::new(204),
      _ => json_reply(404, json!({"message": format!("no route {method} {path}")})),
    }
  }
}

async fn peer(state: Arc<PeerState>) -> Result<MockServer, Box<dyn std::error::Error>> {
  let server = MockServer::start().await;
  Mock::given(wiremock::matchers::any())
    .respond_with(Peer(state))
    .mount(&server)
    .await;
  Ok(server)
}

/// A temp home with a stub supervisor on PATH.
fn home() -> Result<(tempfile::TempDir, String), Box<dyn std::error::Error>> {
  let home = tempfile::TempDir::new()?;
  let path_env = nonroot_shims(home.path())?;
  let (bin, log, _) = shim_env(home.path())?;
  write_shim(
    &bin,
    if cfg!(target_os = "macos") {
      "launchctl"
    } else {
      "systemctl"
    },
    RECORD_OK,
  )?;
  let _ = log;
  Ok((home, path_env))
}

/// Run `setup` unattended against `server`.
fn run_setup(
  home: &Path,
  path_env: &str,
  server: &MockServer,
  extra: &[&str],
) -> Result<Output, Box<dyn std::error::Error>> {
  let url = server.uri();
  let output = Command::new(env!("CARGO_BIN_EXE_toolu-runner"))
    .env("HOME", home)
    .env("TOOLU_RUNNER_HOME", home)
    .env("TOOLU_RUNNER_NO_KEYRING", "1")
    .env("TOOLU_RUNNER_TOKEN", "setup-test-token")
    .env("TOOLU_RUNNER_ONLINE_TIMEOUT_MS", "10000")
    .env("NO_COLOR", "1")
    .env("PATH", path_env)
    .env("SHIM_LOG", home.join("shim.log"))
    .stdin(Stdio::null())
    .args(["setup", "--github-url", &url])
    .args(extra)
    .output()?;
  Ok(output)
}

fn output_text(output: &Output) -> String {
  format!(
    "{}{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  )
}

/// Files under `home` as (relative path, bytes), skipping `_diag`
/// (tracing logs are not setup state).
fn snapshot(home: &Path) -> Vec<(String, Vec<u8>)> {
  let mut files = Vec::new();
  let mut stack = vec![home.to_path_buf()];
  while let Some(dir) = stack.pop() {
    let Ok(entries) = std::fs::read_dir(&dir) else {
      continue;
    };
    for entry in entries.flatten() {
      let path = entry.path();
      let rel = path
        .strip_prefix(home)
        .unwrap_or(&path)
        .to_string_lossy()
        .into_owned();
      if rel.split('/').any(|part| part == "_diag") {
        continue;
      }
      if path.is_dir() {
        stack.push(path);
      } else if let Ok(bytes) = std::fs::read(&path) {
        files.push((rel, bytes));
      }
    }
  }
  files.sort();
  files
}

fn org_config(home: &Path) -> PathBuf {
  home.join("orgs/acme/config.toml")
}

fn unit_file(home: &Path) -> PathBuf {
  if cfg!(target_os = "macos") {
    home.join("Library/LaunchAgents/sh.toolu.runner.plist")
  } else {
    home.join(".config/systemd/user/toolu-runner.service")
  }
}

const ORG_FLAGS: &[&str] = &[
  "--source", "github", "--org", "acme", "--auth", "token", "--name", NAME, "--yes",
];

#[tokio::test]
async fn unattended_org_token_registers_and_reports_online() -> TestResult {
  let (home, path_env) = home()?;
  let server = peer(PeerState::new()).await?;
  let output = run_setup(home.path(), &path_env, &server, ORG_FLAGS)?;
  let text = output_text(&output);
  assert_eq!(output.status.code(), Some(0), "{text}");
  assert!(text.contains("setup: online"), "{text}");
  let runs_on = format!(
    "runs-on: self-hosted, {}, {}",
    shared::platform::runner_os(),
    shared::platform::runner_arch()
  );
  assert!(text.contains(&runs_on), "{text}");
  assert!(text.contains("toolu-runner doctor"), "{text}");
  assert!(org_config(home.path()).exists(), "org registration written");
  assert!(
    unit_file(home.path()).exists(),
    "machine unit written: {text}"
  );
  let cfg = config::config::load_config(&org_config(home.path()))?;
  assert_eq!(cfg.runner_name, NAME);
  Ok(())
}

#[tokio::test]
async fn missing_flag_exits_2_and_writes_nothing() -> TestResult {
  let (home, path_env) = home()?;
  let server = peer(PeerState::new()).await?;
  let before = snapshot(home.path());
  let output = run_setup(home.path(), &path_env, &server, &["--yes"])?;
  let text = output_text(&output);
  assert_eq!(output.status.code(), Some(2), "{text}");
  assert!(text.contains("--org"), "{text}");
  assert_eq!(snapshot(home.path()), before, "no state written");
  Ok(())
}

#[tokio::test]
async fn dry_run_writes_nothing() -> TestResult {
  let (home, path_env) = home()?;
  let server = peer(PeerState::new()).await?;
  let before = snapshot(home.path());
  let mut flags = ORG_FLAGS.to_vec();
  flags.retain(|f| *f != "--yes");
  flags.push("--dry-run");
  let output = run_setup(home.path(), &path_env, &server, &flags)?;
  let text = output_text(&output);
  assert_eq!(output.status.code(), Some(0), "{text}");
  assert!(text.contains("target:"), "review printed: {text}");
  assert!(
    text.contains("runner group:"),
    "review shows the group: {text}"
  );
  assert!(text.contains("dry run"), "{text}");
  assert_eq!(snapshot(home.path()), before, "dry run wrote state");
  Ok(())
}

#[tokio::test]
async fn keep_as_is_changes_nothing() -> TestResult {
  let (home, path_env) = home()?;
  let server = peer(PeerState::new()).await?;
  let first = run_setup(home.path(), &path_env, &server, ORG_FLAGS)?;
  assert_eq!(first.status.code(), Some(0), "{}", output_text(&first));
  let before = snapshot(home.path());
  let second = run_setup(home.path(), &path_env, &server, ORG_FLAGS)?;
  let text = output_text(&second);
  assert_eq!(second.status.code(), Some(0), "{text}");
  assert!(text.contains("Nothing to change."), "{text}");
  assert_eq!(snapshot(home.path()), before, "keep-as-is changed files");
  Ok(())
}

#[tokio::test]
async fn personal_two_repos_one_service() -> TestResult {
  let (home, path_env) = home()?;
  let server = peer(PeerState::new()).await?;
  let output = run_setup(
    home.path(),
    &path_env,
    &server,
    &[
      "--source", "github", "--repo", "octo/web", "--repo", "octo/api", "--auth", "token",
      "--name", NAME, "--yes",
    ],
  )?;
  let text = output_text(&output);
  assert_eq!(output.status.code(), Some(0), "{text}");
  assert!(home.path().join("runners/octo/web/config.toml").exists());
  assert!(home.path().join("runners/octo/api/config.toml").exists());
  assert!(unit_file(home.path()).exists(), "one machine unit: {text}");
  Ok(())
}

#[tokio::test]
async fn bad_org_warns_before_credentials() -> TestResult {
  let (home, path_env) = home()?;
  let server = peer(PeerState::new()).await?;
  let output = run_setup(
    home.path(),
    &path_env,
    &server,
    &["--org", "ghost", "--auth", "token", "--name", NAME, "--yes"],
  )?;
  let text = output_text(&output);
  assert_eq!(output.status.code(), Some(2), "{text}");
  assert!(text.contains("--org"), "{text}");
  assert!(!org_config(home.path()).exists());
  Ok(())
}

#[tokio::test]
async fn name_taken_needs_replace() -> TestResult {
  let (home, path_env) = home()?;
  let state = PeerState::new();
  state.name_taken.store(true, Ordering::SeqCst);
  let server = peer(state).await?;
  let output = run_setup(home.path(), &path_env, &server, ORG_FLAGS)?;
  let text = output_text(&output);
  assert_eq!(output.status.code(), Some(2), "{text}");
  assert!(text.contains("--replace"), "{text}");
  let mut flags = ORG_FLAGS.to_vec();
  flags.push("--replace");
  let output = run_setup(home.path(), &path_env, &server, &flags)?;
  let text2 = output_text(&output);
  assert_eq!(output.status.code(), Some(0), "{text2}");
  Ok(())
}

#[tokio::test]
async fn add_another_keeps_the_first() -> TestResult {
  let (home, path_env) = home()?;
  let server = peer(PeerState::new()).await?;
  let first = run_setup(home.path(), &path_env, &server, ORG_FLAGS)?;
  assert_eq!(first.status.code(), Some(0), "{}", output_text(&first));
  let first_config = std::fs::read(org_config(home.path()))?;
  let unit_before =
    std::fs::read(unit_file(home.path())).expect("unit file must exist after first setup");
  let second = run_setup(
    home.path(),
    &path_env,
    &server,
    &[
      "--repo", "octo/web", "--auth", "token", "--name", NAME, "--yes",
    ],
  )?;
  let text = output_text(&second);
  assert_eq!(second.status.code(), Some(0), "{text}");
  assert!(home.path().join("runners/octo/web/config.toml").exists());
  assert_eq!(
    std::fs::read(org_config(home.path()))?,
    first_config,
    "first registration untouched"
  );
  assert_eq!(
    std::fs::read(unit_file(home.path())).expect("unit file must still exist after second setup"),
    unit_before,
    "same machine unit"
  );
  Ok(())
}

#[tokio::test]
async fn no_service_skips_online_poll() -> TestResult {
  let (home, path_env) = home()?;
  let server = peer(PeerState::new()).await?;
  let mut flags = ORG_FLAGS.to_vec();
  flags.push("--no-service");
  let output = run_setup(home.path(), &path_env, &server, &flags)?;
  let text = output_text(&output);
  assert_eq!(output.status.code(), Some(0), "{text}");
  assert!(text.contains("--no-service"), "{text}");
  assert!(!unit_file(home.path()).exists());
  assert!(org_config(home.path()).exists());
  Ok(())
}

#[tokio::test]
async fn ghes_app_auth_names_token() -> TestResult {
  let (home, path_env) = home()?;
  let server = peer(PeerState::new()).await?;
  let output = Command::new(env!("CARGO_BIN_EXE_toolu-runner"))
    .env("HOME", home.path())
    .env("TOOLU_RUNNER_HOME", home.path())
    .env("TOOLU_RUNNER_NO_KEYRING", "1")
    .env("NO_COLOR", "1")
    .env("PATH", &path_env)
    .stdin(Stdio::null())
    .args([
      "setup",
      "--github-url",
      "https://ghes.example.com",
      "--org",
      "acme",
      "--auth",
      "app",
      "--name",
      NAME,
      "--yes",
    ])
    .output()?;
  let _ = server;
  let text = output_text(&output);
  assert_eq!(output.status.code(), Some(2), "{text}");
  assert!(text.contains("--auth token"), "{text}");
  Ok(())
}

#[tokio::test]
async fn groups_fallback_uses_default_and_names_flag() -> TestResult {
  let (home, path_env) = home()?;
  let state = PeerState::new();
  state.groups_fail.store(true, Ordering::SeqCst);
  let server = peer(state).await?;
  let output = run_setup(home.path(), &path_env, &server, ORG_FLAGS)?;
  let text = output_text(&output);
  assert_eq!(output.status.code(), Some(0), "{text}");
  assert!(text.contains("--runner-group"), "{text}");
  Ok(())
}

#[tokio::test]
async fn partial_apply_resumes() -> TestResult {
  let (home, path_env) = home()?;
  let state = PeerState::new();
  state.fail_api_generate.store(true, Ordering::SeqCst);
  let server = peer(Arc::clone(&state)).await?;
  let flags = [
    "--repo", "octo/web", "--repo", "octo/api", "--auth", "token", "--name", NAME, "--yes",
  ];
  let first = run_setup(home.path(), &path_env, &server, &flags)?;
  assert_eq!(first.status.code(), Some(1), "{}", output_text(&first));
  assert!(home.path().join("runners/octo/web/config.toml").exists());
  assert!(!home.path().join("runners/octo/api/config.toml").exists());
  state.fail_api_generate.store(false, Ordering::SeqCst);
  let second = run_setup(home.path(), &path_env, &server, &flags)?;
  let text = output_text(&second);
  assert_eq!(second.status.code(), Some(0), "{text}");
  assert!(home.path().join("runners/octo/api/config.toml").exists());
  Ok(())
}

/// Drive the binary under `script(1)`: stdin is a file (the
/// `curl | bash` shape), the controlling terminal is the pty, and each
/// `(marker, keys)` step waits until `marker` shows on the pty, then
/// writes `keys`. Returns (exit code, all terminal output).
#[cfg(target_os = "linux")]
fn run_pty(
  home: &Path,
  path_env: &str,
  args: &[&str],
  steps: &[(&str, &[u8])],
) -> Result<(Option<i32>, String), Box<dyn std::error::Error>> {
  run_pty_env(home, path_env, args, steps, &[])
}

/// [`run_pty`] with extra environment `envs` (the toolu.sh tests, #207).
#[cfg(target_os = "linux")]
pub(crate) fn run_pty_env(
  home: &Path,
  path_env: &str,
  args: &[&str],
  steps: &[(&str, &[u8])],
  envs: &[(&str, &str)],
) -> Result<(Option<i32>, String), Box<dyn std::error::Error>> {
  let answers = home.join("answers.txt");
  std::fs::write(&answers, b"")?;
  let cmdline = format!(
    "{} setup {} < {}",
    env!("CARGO_BIN_EXE_toolu-runner"),
    args.join(" "),
    answers.display()
  );
  let mut cmd = Command::new("script");
  cmd
    .args(["-qec", &cmdline, "/dev/null"])
    .env("HOME", home)
    .env("TOOLU_RUNNER_HOME", home)
    .env("TOOLU_RUNNER_NO_KEYRING", "1")
    .env("TOOLU_RUNNER_TOKEN", "setup-test-token")
    .env("TOOLU_RUNNER_ONLINE_TIMEOUT_MS", "10000")
    .env("PATH", path_env)
    .env("SHIM_LOG", home.join("shim.log"))
    .envs(envs.iter().copied())
    .env_remove("NO_COLOR");
  let steps: Vec<(&str, PtyKeys<'_>)> = steps
    .iter()
    .map(|(marker, keys)| (*marker, PtyKeys::Bytes(keys)))
    .collect();
  drive_pty(&mut cmd, &steps)
}

/// What to type once a pty step's marker shows.
#[cfg(target_os = "linux")]
pub(crate) enum PtyKeys<'a> {
  /// These bytes.
  Bytes(&'a [u8]),
  /// Bytes computed from the terminal output so far (a printed link).
  From(&'a dyn Fn(&str) -> Vec<u8>),
}

/// Spawn `script` (already given its `-qec <cmdline> /dev/null` and env)
/// and, for each `(marker, keys)` step, wait until `marker` shows on the
/// pty, then type `keys`. Returns (exit code, all terminal output).
#[cfg(target_os = "linux")]
pub(crate) fn drive_pty(
  script: &mut Command,
  steps: &[(&str, PtyKeys<'_>)],
) -> Result<(Option<i32>, String), Box<dyn std::error::Error>> {
  let mut child: std::process::Child = script
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()?;
  let mut stdout = child.stdout.take().ok_or("no stdout")?;
  let captured = Arc::new(Mutex::new(Vec::<u8>::new()));
  let sink = Arc::clone(&captured);
  let reader = std::thread::spawn(move || {
    let mut buf = [0_u8; 4096];
    loop {
      match stdout.read(&mut buf) {
        Ok(0) | Err(_) => return,
        Ok(n) => {
          if let Ok(mut held) = sink.lock() {
            held.extend(buf.iter().take(n));
          }
        },
      }
    }
  });
  let mut stdin = child.stdin.take().ok_or("no stdin")?;
  let text = |captured: &Arc<Mutex<Vec<u8>>>| -> String {
    captured
      .lock()
      .map(|held| String::from_utf8_lossy(&held).into_owned())
      .unwrap_or_default()
  };
  for (marker, keys) in steps {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !text(&captured).contains(marker) {
      if Instant::now() > deadline {
        let held = text(&captured);
        let _ = child.kill();
        return Err(format!("pty marker {marker:?} never appeared; output: {held}").into());
      }
      std::thread::sleep(Duration::from_millis(100));
    }
    let bytes = match keys {
      PtyKeys::Bytes(bytes) => bytes.to_vec(),
      PtyKeys::From(compute) => compute(&text(&captured)),
    };
    stdin.write_all(&bytes)?;
    stdin.flush()?;
  }
  let status = child.wait()?;
  let _ = reader.join();
  Ok((status.code(), text(&captured)))
}

/// AC-9 (#205): an interactive headless `setup --auth app` no longer steers
/// away from creating the App — it shows the relay link (screen 06); `p`
/// typed on the terminal reaches the wait through setup's own `/dev/tty`
/// (stdin here is a file); Ctrl-C there exits 130 with nothing saved.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn headless_create_reaches_the_relay_link() -> TestResult {
  let (home, path_env) = home()?;
  let server = peer(PeerState::new()).await?;
  let url = server.uri();
  let api = format!("{url}/api/v3");
  let closed = std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?;
  let relay = format!("http://{closed}");
  let (code, output) = run_pty_env(
    home.path(),
    &path_env,
    &[
      "--github-url",
      &url,
      "--org",
      "acme",
      "--auth",
      "app",
      "--headless",
    ],
    &[
      ("p paste code", b"p\n"),
      ("Paste the code from the callback page", b"\x03"),
    ],
    &[
      ("TOOLU_RUNNER_GITHUB_API_URL", &api),
      ("TOOLU_RUNNER_RELAY_URL", &relay),
    ],
  )?;
  assert_eq!(code, Some(130), "output: {output}");
  assert!(!output.contains("needs a browser"), "old steer: {output}");
  assert!(output.contains(&format!("{relay}/gh/app#")), "{output}");
  assert!(!home.path().join("github-app.json").exists());
  Ok(())
}

/// #205: a personal App installs only on its own account, so creating one
/// for repositories an organization owns is refused before anything exists.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn app_for_org_owned_repos_is_refused_before_creation() -> TestResult {
  let (home, path_env) = home()?;
  let server = peer(PeerState::new()).await?;
  let url = server.uri();
  let api = format!("{url}/api/v3");
  let (code, output) = run_pty_env(
    home.path(),
    &path_env,
    &[
      "--github-url",
      &url,
      "--repo",
      "acme/web",
      "--auth",
      "app",
      "--headless",
    ],
    &[],
    &[
      ("TOOLU_RUNNER_GITHUB_API_URL", &api),
      ("TOOLU_RUNNER_RELAY_URL", "http://127.0.0.1:9"),
    ],
  )?;
  assert_eq!(code, Some(2), "output: {output}");
  assert!(output.contains("acme is an organization"), "{output}");
  assert!(!output.contains("/gh/app#"), "no link may print: {output}");
  assert!(!home.path().join("github-app.json").exists());
  Ok(())
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn sigint_at_details_prompt_exits_130() -> TestResult {
  let (home, path_env) = home()?;
  let server = peer(PeerState::new()).await?;
  let before = snapshot(home.path());
  let url = server.uri();
  // No --name: the runner-details prompt asks for it. Ctrl+C there.
  let (code, output) = run_pty(
    home.path(),
    &path_env,
    &["--github-url", &url, "--org", "acme", "--auth", "token"],
    &[("Runner group", b"\r"), ("Runner name", b"\x03")],
  )?;
  assert_eq!(code, Some(130), "output: {output}");
  let after: Vec<(String, Vec<u8>)> = snapshot(home.path())
    .into_iter()
    .filter(|(name, _)| name != "answers.txt")
    .collect();
  assert_eq!(after, before, "SIGINT before Review wrote state: {output}");
  Ok(())
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn remove_via_menu_exits_after_removal() -> TestResult {
  let (home, path_env) = home()?;
  let server = peer(PeerState::new()).await?;
  let first = run_setup(home.path(), &path_env, &server, ORG_FLAGS)?;
  assert_eq!(first.status.code(), Some(0), "{}", output_text(&first));
  let url = server.uri();
  let (code, output) = run_pty(
    home.path(),
    &path_env,
    &["--github-url", &url],
    &[
      ("What would you like to do?", b"\x1b[B\x1b[B\x1b[B\r"),
      ("Which runner?", b"\r"),
    ],
  )?;
  assert_eq!(code, Some(0), "output: {output}");
  assert!(
    !org_config(home.path()).exists(),
    "registration removed: {output}"
  );
  Ok(())
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn reconfigure_preserves_network() -> TestResult {
  let (home, path_env) = home()?;
  let server = peer(PeerState::new()).await?;
  let first = run_setup(home.path(), &path_env, &server, ORG_FLAGS)?;
  assert_eq!(first.status.code(), Some(0), "{}", output_text(&first));
  // Give the registration a [network] section the reconfigure must keep,
  // plus a stored runner group the reconfigure must not re-prompt for.
  let config_path = org_config(home.path());
  let mut cfg = config::config::load_config(&config_path)?;
  cfg.network.no_proxy = vec!["markers.example.com".to_owned()];
  cfg.runner_group = "Default".to_owned();
  config::config::save_config(&config_path, &cfg)?;
  // Caller flags on the reconfigure invocation must survive the menu:
  // --no-service (unit not recreated), --labels (additive), --work-dir.
  std::fs::remove_file(unit_file(home.path()))?;
  let work = home.path().join("reconfigured-work");
  let work_arg = work.to_string_lossy().into_owned();
  let url = server.uri();
  let (code, output) = run_pty(
    home.path(),
    &path_env,
    &[
      "--github-url",
      &url,
      "--no-service",
      "--labels",
      "reconfigured",
      "--work-dir",
      &work_arg,
    ],
    &[
      ("What would you like to do?", b"\x1b[B\x1b[B\r"),
      ("Which runner?", b"\r"),
      ("Apply this setup?", b"\r"),
    ],
  )?;
  assert_eq!(code, Some(0), "output: {output}");
  let after = config::config::load_config(&config_path)?;
  assert_eq!(
    after.network.no_proxy,
    vec!["markers.example.com".to_owned()],
    "[network] preserved through reconfigure"
  );
  assert_eq!(after.runner_group, "Default", "stored group kept: {output}");
  assert!(
    after.labels.iter().any(|l| l == "reconfigured")
      && after.labels.iter().any(|l| l == "self-hosted"),
    "caller labels additive over defaults: {:?}",
    after.labels
  );
  assert_eq!(after.runtime.work_dir, work_arg, "caller --work-dir kept");
  assert!(
    !unit_file(home.path()).exists(),
    "caller --no-service kept: unit must not be recreated"
  );
  Ok(())
}

#[tokio::test]
async fn token_probe_rejects_non_admin() -> TestResult {
  let (home_dir, path_env) = home()?;
  let state = PeerState::new();
  state.admin.store(false, Ordering::SeqCst);
  let server = peer(state).await?;
  let output = run_setup(home_dir.path(), &path_env, &server, ORG_FLAGS)?;
  let text = output_text(&output);
  assert_eq!(output.status.code(), Some(2), "{text}");
  assert!(text.contains("not an admin"), "{text}");
  assert!(!org_config(home_dir.path()).exists());
  Ok(())
}
