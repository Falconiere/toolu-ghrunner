//! `toolu-runner setup`'s toolu.sh branch (#207) through the real binary
//! against `toolu_fixtures::Peer`: toolu.sh's pairing, enrollment and
//! `/runner/host` routes as a stateful peer whose bodies follow the live
//! `dev-console.toolu.sh` captures (`fixtures/toolu_sh/`). Real temp HOME,
//! PATH-stub supervisor; the service's first poll is a real `run --all`.
//! S1–S3 and AC-1..AC-6, AC-9, AC-13, AC-14 of the #207 spec.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;
use tempfile::TempDir;

use crate::install_service_test::{nonroot_shims, shim_env, write_shim};
use crate::supervisor_fixtures::{TestResult, spawn_logged_with, stop, wait_exit};
use crate::toolu_fixtures::{
  Faults, HOST_ID, HOST_TOKEN, HostFault, ORG, Peer, WORKSPACE, write_machine,
};

/// A stub supervisor CLI: log argv, succeed.
const RECORD_OK: &str = "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$SHIM_LOG\"\nexit 0\n";

/// A synthetic single-use enrollment token (`tlu_e_` + 43 characters).
const ENROLL: &str = "tlu_e_enroll207enroll207enroll207enroll207enr";

/// A temp home with stub `systemctl`/`launchctl`, `id`, `loginctl` on PATH.
fn home() -> TestResult<(TempDir, String)> {
  let home = TempDir::new()?;
  let path_env = nonroot_shims(home.path())?;
  let (bin, _, _) = shim_env(home.path())?;
  let supervisor = if cfg!(target_os = "macos") {
    "launchctl"
  } else {
    "systemctl"
  };
  write_shim(&bin, supervisor, RECORD_OK)?;
  Ok((home, path_env))
}

/// The environment every `setup` run here shares: home, peer, stub PATH,
/// plain output, no SSH / display (headless: no browser), verbose logs.
fn envs(home: &Path, path_env: &str, api: &str) -> Vec<(&'static str, String)> {
  vec![
    ("HOME", home.display().to_string()),
    ("TOOLU_RUNNER_HOME", home.display().to_string()),
    ("TOOLU_RUNNER_NO_KEYRING", "1".to_owned()),
    ("TOOLU_RUNNER_TOOLU_API_URL", api.to_owned()),
    ("TOOLU_RUNNER_ONLINE_TIMEOUT_MS", "30000".to_owned()),
    ("TOOLU_RUNNER_ALLOW_VERBOSE", "1".to_owned()),
    ("NO_COLOR", "1".to_owned()),
    ("PATH", path_env.to_owned()),
    ("SHIM_LOG", home.join("shim.log").display().to_string()),
  ]
}

/// Run `setup <args>` unattended (stdin closed) with `extra` env on top.
fn setup(
  home: &Path,
  path_env: &str,
  api: &str,
  args: &[&str],
  extra: &[(&str, &str)],
) -> TestResult<Output> {
  let mut cmd = Command::new(env!("CARGO_BIN_EXE_toolu-runner"));
  cmd
    .arg("setup")
    .args(args)
    .envs(envs(home, path_env, api))
    .env_remove("TOOLU_RUNNER_TOKEN")
    .env_remove("SSH_CONNECTION")
    .env_remove("SSH_TTY")
    .env_remove("DISPLAY")
    .env_remove("WAYLAND_DISPLAY")
    .envs(extra.iter().copied())
    .stdin(Stdio::null());
  Ok(cmd.output()?)
}

fn text(output: &Output) -> String {
  format!(
    "{}{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  )
}

/// The machine's registration dir.
fn machine_dir(home: &Path) -> PathBuf {
  home.join("toolu").join(ORG)
}

/// Every file under `home` (relative path, bytes), skipping the stub bin
/// and its log.
fn files(home: &Path) -> Vec<(String, Vec<u8>)> {
  let mut found = Vec::new();
  let mut stack = vec![home.to_path_buf()];
  while let Some(dir) = stack.pop() {
    for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
      let path = entry.path();
      let rel = path
        .strip_prefix(home)
        .unwrap_or(&path)
        .to_string_lossy()
        .into_owned();
      if rel.starts_with("shim") {
        continue;
      }
      if path.is_dir() {
        stack.push(path);
      } else if let Ok(bytes) = std::fs::read(&path) {
        found.push((rel, bytes));
      }
    }
  }
  found.sort();
  found
}

/// Files that are setup state (not logs).
fn state(home: &Path) -> Vec<(String, Vec<u8>)> {
  files(home)
    .into_iter()
    .filter(|(rel, _)| !rel.split('/').any(|part| part == "_diag"))
    .collect()
}

/// AC-5: the host token is in `credentials.json` and nowhere else — not in
/// any other file under the home (logs included) nor in `printed`; each of
/// `secrets` (a device code, an enrollment token) is nowhere at all.
fn assert_secrets_contained(home: &Path, printed: &str, secrets: &[&str]) {
  let creds = format!("toolu/{ORG}/credentials.json");
  for secret in secrets.iter().chain([&HOST_TOKEN]) {
    assert!(!printed.contains(secret), "{secret} printed: {printed}");
    for (rel, bytes) in files(home) {
      if *secret == HOST_TOKEN && rel == creds {
        continue;
      }
      assert!(
        !String::from_utf8_lossy(&bytes).contains(secret),
        "{secret} leaked into {rel}"
      );
    }
  }
}

/// AC-9: GitHub-only flags and a GitHub `--enroll-token` exit 2 naming the
/// flag and write nothing; `--dry-run` reviews without a request.
#[tokio::test]
async fn flags() -> TestResult<()> {
  let peer = Peer::start(&[], 600_000, Faults::default(), "true").await?;
  let (home, path_env) = home()?;
  let api = peer.server.uri();
  let before = state(home.path());
  for (args, flag) in [
    (vec!["--source", "toolu", "--org", "acme"], "--org"),
    (vec!["--enroll-token", ENROLL, "--ephemeral"], "--ephemeral"),
    (
      vec!["--source", "github", "--enroll-token", ENROLL],
      "--enroll-token",
    ),
  ] {
    let out = setup(home.path(), &path_env, &api, &args, &[])?;
    let printed = text(&out);
    assert_eq!(out.status.code(), Some(2), "{args:?}: {printed}");
    assert!(printed.contains(flag), "{args:?} names {flag}: {printed}");
  }
  let out = setup(
    home.path(),
    &path_env,
    &api,
    &["--source", "toolu", "--dry-run", "--labels", "gpu"],
    &[],
  )?;
  let printed = text(&out);
  assert_eq!(out.status.code(), Some(0), "{printed}");
  assert!(
    printed.contains(&format!("source: toolu.sh ({api})")),
    "{printed}"
  );
  assert!(
    printed.contains("dry run: nothing was written"),
    "{printed}"
  );
  assert!(peer.seen().is_empty(), "requests: {:?}", peer.paths());
  assert_eq!(state(home.path()), before, "{printed}");
  Ok(())
}

/// S1 / AC-1, AC-5: pending → `slow_down` → approved writes the #206
/// registration and a 0600 host token, polls no faster than `interval`
/// (+5 s after `slow_down`), and the token reaches no output or log.
#[tokio::test]
async fn pairing() -> TestResult<()> {
  let faults = Faults {
    pair_scripts: vec![vec!["authorization_pending", "slow_down", "approved"]],
    ..Faults::default()
  };
  let peer = Peer::start(&[], 600_000, faults, "true").await?;
  let (home, path_env) = home()?;
  let api = peer.server.uri();
  let out = setup(
    home.path(),
    &path_env,
    &api,
    &["--source", "toolu", "--no-service", "--labels", "gpu"],
    &[],
  )?;
  let printed = text(&out);
  assert_eq!(out.status.code(), Some(0), "{printed}");
  for want in [
    "go to   127.0.0.1:",
    "enter   PAIR-0001",
    &format!("paired host {HOST_ID} · workspace {WORKSPACE}"),
    "host token saved (0600 · never logged)",
    "runs-on: [toolu-self-hosted]",
  ] {
    assert!(printed.contains(want), "missing {want:?}: {printed}");
  }

  let pair = peer.seen_ending("/runner/pair");
  let sent = &pair.first().ok_or("no pair request")?.body;
  assert_eq!(sent.get("labels"), Some(&serde_json::json!(["gpu"])));
  assert_eq!(
    sent.get("runner_version").and_then(Value::as_str),
    Some(env!("CARGO_PKG_VERSION"))
  );
  let polls = peer.seen_ending("/runner/pair/token");
  assert_eq!(polls.len(), 3, "{:?}", peer.paths());
  let mut at = vec![pair.first().ok_or("no pair")?.at];
  at.extend(polls.iter().map(|poll| poll.at));
  let gaps: Vec<Duration> = at
    .iter()
    .zip(at.iter().skip(1))
    .map(|(before, after)| after.duration_since(*before))
    .collect();
  // pair → poll 1 and poll 1 → poll 2 at `interval` (1 s); after the
  // slow_down (poll 2), interval + 5 s.
  let floors = [1, 1, 6].map(Duration::from_secs);
  assert_eq!(gaps.len(), floors.len(), "{gaps:?}");
  for (gap, floor) in gaps.iter().zip(floors) {
    assert!(*gap >= floor, "a poll came too early: {gaps:?}");
  }

  let dir = machine_dir(home.path());
  let config = std::fs::read_to_string(dir.join("config.toml"))?;
  for want in [
    "kind = \"toolu\"",
    &format!("api_url = \"{api}\""),
    &format!("host_id = \"{HOST_ID}\""),
    &format!("org = \"{ORG}\""),
    "labels = [\"gpu\"]",
  ] {
    assert!(config.contains(want), "config lacks {want}: {config}");
  }
  let creds_path = dir.join("credentials.json");
  let creds: Value = serde_json::from_str(&std::fs::read_to_string(&creds_path)?)?;
  assert_eq!(
    creds.get("access_token").and_then(Value::as_str),
    Some(HOST_TOKEN)
  );
  assert_eq!(
    std::fs::metadata(&creds_path)?.permissions().mode() & 0o777,
    0o600
  );
  assert_secrets_contained(home.path(), &printed, &["devicecode207-1"]);
  Ok(())
}

/// S2 / AC-2: a denied pairing exits 2 and saves nothing.
#[tokio::test]
async fn denied() -> TestResult<()> {
  let faults = Faults {
    pair_scripts: vec![vec!["access_denied"]],
    ..Faults::default()
  };
  let peer = Peer::start(&[], 600_000, faults, "true").await?;
  let (home, path_env) = home()?;
  let before = state(home.path());
  let out = setup(
    home.path(),
    &path_env,
    &peer.server.uri(),
    &["--source", "toolu"],
    &[],
  )?;
  let printed = text(&out);
  assert_eq!(out.status.code(), Some(2), "{printed}");
  assert!(
    printed.contains("Pairing was denied in the console. Nothing was saved."),
    "{printed}"
  );
  assert_eq!(state(home.path()), before, "{printed}");
  Ok(())
}

/// S2 / AC-2: an expired code exits 1 unattended and saves nothing.
#[tokio::test]
async fn expired() -> TestResult<()> {
  let faults = Faults {
    pair_scripts: vec![vec!["expired_token"]],
    ..Faults::default()
  };
  let peer = Peer::start(&[], 600_000, faults, "true").await?;
  let (home, path_env) = home()?;
  let before = state(home.path());
  let out = setup(
    home.path(),
    &path_env,
    &peer.server.uri(),
    &["--source", "toolu"],
    &[],
  )?;
  let printed = text(&out);
  assert_eq!(out.status.code(), Some(1), "{printed}");
  assert!(
    printed.contains("Pairing code expired. Nothing was saved."),
    "{printed}"
  );
  assert_eq!(state(home.path()), before, "{printed}");
  Ok(())
}

/// S3 / AC-3: a valid enrollment token pairs with no question and no
/// pairing code; the same token on a second box is refused (exit 2,
/// nothing written); toolu.sh created exactly one machine.
#[tokio::test]
async fn enroll() -> TestResult<()> {
  let faults = Faults {
    enroll_tokens: vec![ENROLL.to_owned()],
    ..Faults::default()
  };
  let peer = Peer::start(&[], 600_000, faults, "true").await?;
  let api = peer.server.uri();
  let (first, path_env) = home()?;
  let out = setup(
    first.path(),
    &path_env,
    &api,
    &["--enroll-token", ENROLL, "--no-service"],
    &[],
  )?;
  let printed = text(&out);
  assert_eq!(out.status.code(), Some(0), "{printed}");
  assert!(
    machine_dir(first.path()).join("config.toml").exists(),
    "{printed}"
  );
  assert!(
    peer.seen_ending("/runner/pair").is_empty(),
    "{:?}",
    peer.paths()
  );
  assert_secrets_contained(first.path(), &printed, &[ENROLL]);

  let (second, path_env) = home()?;
  let before = state(second.path());
  let out = setup(
    second.path(),
    &path_env,
    &api,
    &["--enroll-token", ENROLL, "--no-service"],
    &[],
  )?;
  let printed = text(&out);
  assert_eq!(out.status.code(), Some(2), "{printed}");
  assert!(
    printed.contains("refused the enrollment token"),
    "{printed}"
  );
  assert_eq!(state(second.path()), before, "{printed}");
  assert_eq!(peer.machines(), 1);
  Ok(())
}

/// AC-4: with the machine service, setup waits for it to poll (a real
/// `run --all` here) and confirms the machine is enabled; the registration
/// is served by a separate process.
#[tokio::test]
async fn service_first_poll() -> TestResult<()> {
  let faults = Faults {
    enroll_tokens: vec![ENROLL.to_owned()],
    ..Faults::default()
  };
  let peer = Peer::start(&[], 600_000, faults, "true").await?;
  let api = peer.server.uri();
  let (home, path_env) = home()?;
  let env = envs(home.path(), &path_env, &api);
  let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
  let setup = spawn_logged_with(
    home.path(),
    "setup",
    &["setup", "--enroll-token", ENROLL],
    &env,
  )?;
  let config = machine_dir(home.path()).join("config.toml");
  let deadline = Instant::now() + Duration::from_secs(30);
  while !config.exists() {
    if Instant::now() > deadline {
      return Err(
        format!(
          "never paired: {}",
          crate::supervisor_fixtures::log_of(home.path(), "setup")
        )
        .into(),
      );
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
  }
  let mut run_env = env.clone();
  run_env.push(("TOOLU_RUNNER_POLL_INTERVAL_MS", "100"));
  let service = spawn_logged_with(home.path(), "service", &["run", "--all"], &run_env)?;
  let out = wait_exit(setup).await?;
  let printed = crate::supervisor_fixtures::log_of(home.path(), "setup");
  assert_eq!(out.status.code(), Some(0), "{printed}");
  for want in [
    "setup: service installed",
    "setup: first poll enabled",
    "runs-on: [toolu-self-hosted]",
  ] {
    assert!(printed.contains(want), "missing {want:?}: {printed}");
  }
  peer.wait_for("/runner/jobs/queued", 1).await?;
  stop(service, home.path(), "service").await?;
  assert_secrets_contained(home.path(), &printed, &[]);
  Ok(())
}

/// AC-6: re-running on a paired box sends `PATCH /runner/host` (labels and
/// version), never pairs or enrolls again, and stores the new labels; an
/// enrollment token given then is not used.
#[tokio::test]
async fn rerun_patches() -> TestResult<()> {
  let faults = Faults {
    enroll_tokens: vec![ENROLL.to_owned(), format!("{ENROLL}2")],
    ..Faults::default()
  };
  let peer = Peer::start(&[], 600_000, faults, "true").await?;
  let api = peer.server.uri();
  let (home, path_env) = home()?;
  let first = setup(
    home.path(),
    &path_env,
    &api,
    &["--enroll-token", ENROLL, "--no-service"],
    &[],
  )?;
  assert_eq!(first.status.code(), Some(0), "{}", text(&first));

  let out = setup(
    home.path(),
    &path_env,
    &api,
    &["--source", "toolu", "--no-service", "--labels", "gpu,arm"],
    &[],
  )?;
  let printed = text(&out);
  assert_eq!(out.status.code(), Some(0), "{printed}");
  assert!(
    printed.contains(&format!("updated host {HOST_ID} · workspace {WORKSPACE}")),
    "{printed}"
  );
  let patches: Vec<_> = peer
    .seen_ending("/runner/host")
    .into_iter()
    .filter(|s| s.method == "PATCH")
    .collect();
  let patch = patches.first().ok_or("no PATCH")?;
  assert_eq!(
    patch.body.get("labels"),
    Some(&serde_json::json!(["gpu", "arm"]))
  );
  assert_eq!(
    patch.body.get("runner_version").and_then(Value::as_str),
    Some(env!("CARGO_PKG_VERSION"))
  );
  assert!(patch.body.get("platform").is_none(), "{:?}", patch.body);
  let config = config::config::load_config(&machine_dir(home.path()).join("config.toml"))?;
  assert_eq!(config.labels, ["gpu", "arm"]);

  let enroll_again = format!("{ENROLL}2");
  let out = setup(
    home.path(),
    &path_env,
    &api,
    &["--enroll-token", &enroll_again, "--no-service"],
    &[],
  )?;
  let printed = text(&out);
  assert_eq!(out.status.code(), Some(0), "{printed}");
  assert!(
    printed.contains("the enrollment token was not used"),
    "{printed}"
  );
  assert_eq!(peer.machines(), 1, "{:?}", peer.paths());
  assert!(peer.seen_ending("/runner/pair").is_empty());
  Ok(())
}

/// AC-13: on a terminal, setup asks for the job source; toolu.sh shows the
/// code and a countdown; an expired code is replaced on enter.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn pty_source_and_new_code() -> TestResult<()> {
  let faults = Faults {
    pair_scripts: vec![vec!["expired_token"], vec!["approved"]],
    ..Faults::default()
  };
  let peer = Peer::start(&[], 600_000, faults, "true").await?;
  let api = peer.server.uri();
  let (home, path_env) = home()?;
  let (code, output) = crate::setup_cmd_test::run_pty_env(
    home.path(),
    &path_env,
    &["--no-service"],
    &[
      ("Where should this runner get jobs from?", b"\x1b[B\r"),
      ("Press enter for a new code", b"\r"),
    ],
    &[
      ("TOOLU_RUNNER_TOOLU_API_URL", api.as_str()),
      ("TOOLU_RUNNER_TOKEN", ""),
    ],
  )?;
  assert_eq!(code, Some(0), "output: {output}");
  for want in [
    "PAIR-0001",
    "expires in",
    "Pairing code expired",
    "PAIR-0002",
    "runs-on: [toolu-self-hosted]",
  ] {
    assert!(output.contains(want), "missing {want:?}: {output}");
  }
  assert_eq!(
    peer.seen_ending("/runner/pair").len(),
    2,
    "{:?}",
    peer.paths()
  );
  assert!(machine_dir(home.path()).join("config.toml").exists());
  Ok(())
}

/// AC-14: a desktop session opens the complete verification URI; a
/// headless (SSH) one never starts a browser.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn browser_open() -> TestResult<()> {
  let faults = Faults {
    pair_scripts: vec![vec!["approved"]],
    ..Faults::default()
  };
  for (desktop, extra) in [
    (true, vec![("DISPLAY", ":0")]),
    (
      false,
      vec![
        ("DISPLAY", ":0"),
        ("SSH_CONNECTION", "10.0.0.1 1 10.0.0.2 22"),
      ],
    ),
  ] {
    let peer = Peer::start(&[], 600_000, faults.clone(), "true").await?;
    let (home, path_env) = home()?;
    let (bin, _, _) = shim_env(home.path())?;
    let opened = home.path().join("opened.log");
    write_shim(
      &bin,
      "xdg-open",
      &format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n",
        opened.display()
      ),
    )?;
    let out = setup(
      home.path(),
      &path_env,
      &peer.server.uri(),
      &["--source", "toolu", "--no-service"],
      &extra,
    )?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let deadline = Instant::now() + Duration::from_secs(if desktop { 10 } else { 2 });
    while !opened.exists() && Instant::now() < deadline {
      tokio::time::sleep(Duration::from_millis(100)).await;
    }
    if desktop {
      let line = std::fs::read_to_string(&opened)?;
      assert!(line.contains("/pair?code=PAIR-0001"), "{line}");
    } else {
      assert!(!opened.exists(), "a headless box opened a browser");
    }
  }
  Ok(())
}

/// A 503 on a pairing poll is retried at the interval, keeping the code.
#[tokio::test]
async fn a_transient_poll_keeps_the_code() -> TestResult<()> {
  let faults = Faults {
    pair_scripts: vec![vec!["503", "approved"]],
    ..Faults::default()
  };
  let peer = Peer::start(&[], 600_000, faults, "true").await?;
  let (home, path_env) = home()?;
  let out = setup(
    home.path(),
    &path_env,
    &peer.server.uri(),
    &["--source", "toolu", "--no-service"],
    &[],
  )?;
  let printed = text(&out);
  assert_eq!(out.status.code(), Some(0), "{printed}");
  assert_eq!(
    peer.seen_ending("/runner/pair").len(),
    1,
    "{:?}",
    peer.paths()
  );
  assert_eq!(peer.seen_ending("/runner/pair/token").len(), 2);
  Ok(())
}

/// toolu.sh without self-hosted machines (its routes answer 404) exits 1
/// naming the origin, writing nothing.
#[tokio::test]
async fn machines_off_on_toolu_exits_1() -> TestResult<()> {
  let off = wiremock::MockServer::start().await;
  let (home, path_env) = home()?;
  let before = state(home.path());
  let out = setup(
    home.path(),
    &path_env,
    &off.uri(),
    &["--source", "toolu"],
    &[],
  )?;
  let printed = text(&out);
  assert_eq!(out.status.code(), Some(1), "{printed}");
  assert!(
    printed.contains(&format!(
      "toolu.sh does not offer self-hosted machines at {}",
      off.uri()
    )),
    "{printed}"
  );
  assert_eq!(state(home.path()), before);
  Ok(())
}

/// The service never polling within the online timeout exits 1, keeping
/// the registration so a re-run resumes.
#[tokio::test]
async fn first_poll_timeout_exits_1_and_keeps_the_pairing() -> TestResult<()> {
  let faults = Faults {
    enroll_tokens: vec![ENROLL.to_owned()],
    ..Faults::default()
  };
  let peer = Peer::start(&[], 600_000, faults, "true").await?;
  let (home, path_env) = home()?;
  let out = setup(
    home.path(),
    &path_env,
    &peer.server.uri(),
    &["--enroll-token", ENROLL],
    &[("TOOLU_RUNNER_ONLINE_TIMEOUT_MS", "1000")],
  )?;
  let printed = text(&out);
  assert_eq!(out.status.code(), Some(1), "{printed}");
  assert!(
    printed.contains("did not start polling within 1s"),
    "{printed}"
  );
  assert!(machine_dir(home.path()).join("config.toml").exists());
  Ok(())
}

/// With `--source toolu`, `TOOLU_RUNNER_ENROLL_TOKEN` stands in for
/// `--enroll-token`, keeping the token out of argv.
#[tokio::test]
async fn the_enrollment_token_can_come_from_the_environment() -> TestResult<()> {
  let faults = Faults {
    enroll_tokens: vec![ENROLL.to_owned()],
    ..Faults::default()
  };
  let peer = Peer::start(&[], 600_000, faults, "true").await?;
  let (home, path_env) = home()?;
  let out = setup(
    home.path(),
    &path_env,
    &peer.server.uri(),
    &["--source", "toolu", "--no-service"],
    &[("TOOLU_RUNNER_ENROLL_TOKEN", ENROLL)],
  )?;
  let printed = text(&out);
  assert_eq!(out.status.code(), Some(0), "{printed}");
  assert_eq!(peer.machines(), 1);
  assert!(
    peer.seen_ending("/runner/pair").is_empty(),
    "{:?}",
    peer.paths()
  );
  assert!(machine_dir(home.path()).join("config.toml").exists());
  Ok(())
}

/// A `TOOLU_RUNNER_ENROLL_TOKEN` left exported never picks the source: a
/// GitHub setup stays GitHub and toolu.sh is never called.
#[tokio::test]
async fn an_exported_enrollment_token_does_not_hijack_github() -> TestResult<()> {
  let peer = Peer::start(&[], 600_000, Faults::default(), "true").await?;
  let (home, path_env) = home()?;
  let out = setup(
    home.path(),
    &path_env,
    &peer.server.uri(),
    &[
      "--org",
      "acme",
      "--github-url",
      "http://127.0.0.1:9",
      "--auth",
      "token",
      "--yes",
    ],
    &[("TOOLU_RUNNER_ENROLL_TOKEN", ENROLL)],
  )?;
  let printed = text(&out);
  // The GitHub path ran: its live org check hit the dead GitHub URL.
  assert_eq!(out.status.code(), Some(1), "{printed}");
  assert!(
    printed.contains("could not check the organization \"acme\" on GitHub"),
    "{printed}"
  );
  assert!(!printed.contains("GitHub-only"), "{printed}");
  assert!(!printed.contains("--enroll-token"), "{printed}");
  assert!(peer.seen().is_empty(), "{:?}", peer.paths());
  assert!(!machine_dir(home.path()).exists());
  Ok(())
}

/// Re-running on a machine revoked in the console exits 2 with the fix.
#[tokio::test]
async fn rerun_on_a_revoked_machine_names_the_fix() -> TestResult<()> {
  let faults = Faults {
    host_fault: Some(HostFault::Revoked),
    ..Faults::default()
  };
  let peer = Peer::start(&[], 600_000, faults, "true").await?;
  let (home, path_env) = home()?;
  let config = write_machine(home.path(), &peer.server.uri(), HOST_TOKEN, "")?;
  let out = setup(
    home.path(),
    &path_env,
    &peer.server.uri(),
    &["--source", "toolu", "--no-service"],
    &[],
  )?;
  let printed = text(&out);
  assert_eq!(out.status.code(), Some(2), "{printed}");
  let fix = format!(
    "toolu-runner remove --config {} && toolu-runner setup --source toolu",
    config.display()
  );
  assert!(printed.contains(&fix), "{printed}");
  assert!(!printed.contains(HOST_TOKEN), "{printed}");
  Ok(())
}

/// Keep as is on a paired box asks toolu.sh about the machine and changes
/// nothing.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn keep_as_is_reads_the_machine() -> TestResult<()> {
  let peer = Peer::start(&[], 600_000, Faults::default(), "true").await?;
  let (home, path_env) = home()?;
  write_machine(home.path(), &peer.server.uri(), HOST_TOKEN, "")?;
  let before = state(home.path());
  let (code, output) = crate::setup_cmd_test::run_pty_env(
    home.path(),
    &path_env,
    &[],
    &[("What would you like to do?", b"\r")],
    &[("TOOLU_RUNNER_TOKEN", "")],
  )?;
  assert_eq!(code, Some(0), "output: {output}");
  assert!(
    output.contains(&format!("host {HOST_ID} · workspace {WORKSPACE} · enabled")),
    "{output}"
  );
  assert!(output.contains("Nothing to change"), "{output}");
  let after: Vec<_> = state(home.path())
    .into_iter()
    .filter(|(rel, _)| rel != "answers.txt")
    .collect();
  assert_eq!(after, before);
  Ok(())
}

/// Keep as is on a revoked machine shows the error row, still checks the
/// next registration (a healthy machine of another org), then exits 2,
/// changing nothing.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn keep_as_is_reports_a_revoked_machine() -> TestResult<()> {
  let faults = Faults {
    host_fault: Some(HostFault::Revoked),
    ..Faults::default()
  };
  let peer = Peer::start(&[], 600_000, faults, "true").await?;
  let (home, path_env) = home()?;
  let config = write_machine(home.path(), &peer.server.uri(), HOST_TOKEN, "")?;
  // A second, healthy machine listed after it (`toolu/zz_org`).
  let healthy = Peer::start(&[], 600_000, Faults::default(), "true").await?;
  let other = home.path().join("toolu").join("zz_org");
  std::fs::create_dir_all(&other)?;
  std::fs::copy(
    config.with_file_name("credentials.json"),
    other.join("credentials.json"),
  )?;
  std::fs::write(
    other.join("config.toml"),
    std::fs::read_to_string(&config)?
      .replace(&peer.server.uri(), &healthy.server.uri())
      .replace(&format!("org = \"{ORG}\""), "org = \"zz_org\"")
      .replace(
        &machine_dir(home.path()).display().to_string(),
        &other.display().to_string(),
      ),
  )?;
  let before = state(home.path());
  let (code, output) = crate::setup_cmd_test::run_pty_env(
    home.path(),
    &path_env,
    &[],
    &[("What would you like to do?", b"\r")],
    &[("TOOLU_RUNNER_TOKEN", "")],
  )?;
  assert_eq!(code, Some(2), "output: {output}");
  let revoked_at = output.find("it was revoked").ok_or("no revoked row")?;
  let healthy_at = output
    .find(&format!("host {HOST_ID} · workspace {WORKSPACE} · enabled"))
    .ok_or("the healthy machine was not checked")?;
  assert!(revoked_at < healthy_at, "{output}");
  assert!(output.contains("it was revoked"), "{output}");
  assert!(
    output.contains(&format!(
      "toolu-runner remove --config {}",
      config.display()
    )),
    "{output}"
  );
  assert!(!output.contains("Nothing to change"), "{output}");
  let after: Vec<_> = state(home.path())
    .into_iter()
    .filter(|(rel, _)| rel != "answers.txt")
    .collect();
  assert_eq!(after, before);
  Ok(())
}

/// toolu.sh's live refusal of a `toolu-` label (`pair_reserved_label_400.json`).
const RESERVED: &str = "labels in toolu's reserved \"toolu-\" namespace: toolu-gpu";

/// A reserved label is refused (400) by both the pairing and the enrollment
/// route: exit 2 naming toolu.sh's reason, nothing written, no machine.
#[tokio::test]
async fn a_reserved_label_is_refused_and_nothing_is_written() -> TestResult<()> {
  let faults = Faults {
    enroll_tokens: vec![ENROLL.to_owned()],
    ..Faults::default()
  };
  let peer = Peer::start(&[], 600_000, faults, "true").await?;
  let (home, path_env) = home()?;
  let before = state(home.path());
  for (args, what) in [
    (vec!["--source", "toolu"], "start pairing"),
    (vec!["--enroll-token", ENROLL], "enroll"),
  ] {
    let mut args = args;
    args.extend(["--no-service", "--labels", "toolu-gpu"]);
    let out = setup(home.path(), &path_env, &peer.server.uri(), &args, &[])?;
    let printed = text(&out);
    assert_eq!(out.status.code(), Some(2), "{args:?}: {printed}");
    assert!(
      printed.contains(&format!("toolu.sh refused to {what}: {RESERVED}")),
      "{args:?}: {printed}"
    );
    assert_eq!(state(home.path()), before, "{args:?}: {printed}");
  }
  assert_eq!(peer.machines(), 0, "{:?}", peer.paths());
  Ok(())
}

/// A re-run whose labels toolu.sh refuses (PATCH 400) exits 2 and keeps the
/// stored registration as it was.
#[tokio::test]
async fn a_refused_label_update_keeps_the_registration() -> TestResult<()> {
  let peer = Peer::start(&[], 600_000, Faults::default(), "true").await?;
  let (home, path_env) = home()?;
  write_machine(home.path(), &peer.server.uri(), HOST_TOKEN, "")?;
  let before = state(home.path());
  let out = setup(
    home.path(),
    &path_env,
    &peer.server.uri(),
    &["--source", "toolu", "--no-service", "--labels", "toolu-gpu"],
    &[],
  )?;
  let printed = text(&out);
  assert_eq!(out.status.code(), Some(2), "{printed}");
  assert!(
    printed.contains(&format!(
      "toolu.sh refused to update the machine: {RESERVED}"
    )),
    "{printed}"
  );
  assert_eq!(state(home.path()), before, "{printed}");
  Ok(())
}

/// Enrollment against toolu.sh without self-hosted machines (404) or with a
/// reply outside the contract exits 1, writing nothing.
#[tokio::test]
async fn enrollment_without_machines_or_with_a_bad_reply_exits_1() -> TestResult<()> {
  let off = wiremock::MockServer::start().await;
  let malformed = wiremock::MockServer::start().await;
  wiremock::Mock::given(wiremock::matchers::method("POST"))
    .and(wiremock::matchers::path("/runner/enroll"))
    .respond_with(wiremock::ResponseTemplate::new(200).set_body_string("{\"ok\":true}"))
    .mount(&malformed)
    .await;
  let (home, path_env) = home()?;
  let before = state(home.path());
  for (api, want) in [
    (
      off.uri(),
      format!(
        "toolu.sh does not offer self-hosted machines at {} (enroll: 404)",
        off.uri()
      ),
    ),
    (malformed.uri(), "could not enroll with toolu.sh".to_owned()),
  ] {
    let out = setup(
      home.path(),
      &path_env,
      &api,
      &["--enroll-token", ENROLL, "--no-service"],
      &[],
    )?;
    let printed = text(&out);
    assert_eq!(out.status.code(), Some(1), "{api}: {printed}");
    assert!(printed.contains(&want), "{api}: {printed}");
    assert_eq!(state(home.path()), before, "{api}: {printed}");
  }
  Ok(())
}

/// A pairing poll answered outside the contract (an unknown `error`, or a
/// 404) exits 1 and saves nothing.
#[tokio::test]
async fn a_pairing_poll_outside_the_contract_exits_1() -> TestResult<()> {
  for (step, want) in [
    (
      "server_error",
      "toolu.sh refused the pairing poll: server_error",
    ),
    (
      "404",
      "polling the pairing failed: toolu.sh answered 404 (no such job, or self-hosted machines \
       are off)",
    ),
  ] {
    let faults = Faults {
      pair_scripts: vec![vec![step]],
      ..Faults::default()
    };
    let peer = Peer::start(&[], 600_000, faults, "true").await?;
    let (home, path_env) = home()?;
    let before = state(home.path());
    let out = setup(
      home.path(),
      &path_env,
      &peer.server.uri(),
      &["--source", "toolu", "--no-service"],
      &[],
    )?;
    let printed = text(&out);
    assert_eq!(out.status.code(), Some(1), "{step}: {printed}");
    assert!(printed.contains(want), "{step}: {printed}");
    assert_eq!(state(home.path()), before, "{step}: {printed}");
  }
  Ok(())
}

/// A machine revoked between approval and its first `GET /runner/host`
/// exits 2 with the remove + setup fix.
#[tokio::test]
async fn a_machine_revoked_right_after_pairing_names_the_fix() -> TestResult<()> {
  let faults = Faults {
    pair_scripts: vec![vec!["approved"]],
    host_fault: Some(HostFault::Revoked),
    ..Faults::default()
  };
  let peer = Peer::start(&[], 600_000, faults, "true").await?;
  let (home, path_env) = home()?;
  let out = setup(
    home.path(),
    &path_env,
    &peer.server.uri(),
    &["--source", "toolu", "--no-service"],
    &[],
  )?;
  let printed = text(&out);
  assert_eq!(out.status.code(), Some(2), "{printed}");
  let fix = format!(
    "toolu-runner remove --config {} && toolu-runner setup --source toolu",
    machine_dir(home.path()).join("config.toml").display()
  );
  assert!(printed.contains(&fix), "{printed}");
  assert_secrets_contained(home.path(), &printed, &["devicecode207-1"]);
  Ok(())
}

/// Remove in the re-run menu revokes a toolu.sh machine with its own host
/// token (`DELETE /runner/host`, no GitHub bearer) and deletes its state.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn the_menu_removes_a_toolu_machine() -> TestResult<()> {
  let peer = Peer::start(&[], 600_000, Faults::default(), "true").await?;
  let (home, path_env) = home()?;
  let config = write_machine(home.path(), &peer.server.uri(), HOST_TOKEN, "")?;
  let (code, output) = crate::setup_cmd_test::run_pty_env(
    home.path(),
    &path_env,
    &[],
    &[
      ("What would you like to do?", b"\x1b[B\x1b[B\x1b[B\r"),
      ("Which runner?", b"\r"),
    ],
    &[("TOOLU_RUNNER_TOKEN", "")],
  )?;
  assert_eq!(code, Some(0), "output: {output}");
  let deletes: Vec<_> = peer
    .seen_ending("/runner/host")
    .into_iter()
    .filter(|s| s.method == "DELETE")
    .collect();
  assert_eq!(deletes.len(), 1, "{:?}", peer.paths());
  assert_eq!(
    deletes.first().and_then(|s| s.authorization.as_deref()),
    Some(format!("Bearer {HOST_TOKEN}").as_str())
  );
  assert!(!config.exists(), "{output}");
  assert!(!config.with_file_name("credentials.json").exists());
  Ok(())
}

/// Reconfigure in the re-run menu refreshes a toolu.sh machine with
/// `PATCH /runner/host`; it never pairs again.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn the_menu_reconfigures_a_toolu_machine() -> TestResult<()> {
  let peer = Peer::start(&[], 600_000, Faults::default(), "true").await?;
  let (home, path_env) = home()?;
  write_machine(home.path(), &peer.server.uri(), HOST_TOKEN, "")?;
  let (code, output) = crate::setup_cmd_test::run_pty_env(
    home.path(),
    &path_env,
    &["--no-service"],
    &[
      ("What would you like to do?", b"\x1b[B\x1b[B\r"),
      ("Which runner?", b"\r"),
    ],
    &[("TOOLU_RUNNER_TOKEN", "")],
  )?;
  assert_eq!(code, Some(0), "output: {output}");
  assert!(
    output.contains(&format!("updated: host {HOST_ID} · workspace {WORKSPACE}")),
    "{output}"
  );
  let methods: Vec<String> = peer.seen().into_iter().map(|s| s.method).collect();
  assert_eq!(methods, ["PATCH"], "{:?}", peer.paths());
  assert!(peer.seen_ending("/runner/pair").is_empty());
  Ok(())
}
