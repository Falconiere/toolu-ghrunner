//! Zero-arg `register` e2e (spec 2026-07-12-zero-arg-register-design):
//! per-repo layout + persistence (AC-3 / AC-12), inference gating for
//! non-github.com and non-repo cwds (AC-4), the non-TTY token gate after a
//! successful inference, and the home-root shared token store (AC-6).
//!
//! Every test shells out the debug binary (`CARGO_BIN_EXE_toolu-runner`)
//! with `TOOLU_RUNNER_HOME` (and `HOME`) pointed at a fresh tempdir. The
//! register HTTP fixture is the committed real-shaped `generate-jitconfig`
//! response (`fixtures/generate_jitconfig_response.json`), whose
//! `encoded_jit_config` is a genuine parseable 3-blob JIT envelope, served
//! by a local wiremock server — no real network, no mock-data stand-ins.
//! Inference tests run against real `git init` repos.
//!
//! Token-store tests guard on the backend `AuthStore::new` picks: the
//! `File` backend is hermetic per tempdir home; a reachable OS keyring is
//! machine-global (seeding or reading it from tests would touch — or leak
//! from — the developer's real store), so those tests skip on keyring
//! machines and run fully on keyless environments such as the Linux CI
//! runner (same assumption `live_login_e2e.rs` documents).

use std::path::Path;

use config::auth_store::{AuthStore, StoredToken};
use config::config::{load_config, load_credentials};
use wiremock::matchers::{header, method, path as request_path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Committed real-shaped `generate-jitconfig` 201 body (same fixture as
/// `gh_compat_register.rs`): `runner.id` 461, and the envelope's decoded
/// `.credentials` blob carries `toolu-runner-fixture-client`.
const RESPONSE_FIXTURE: &str = include_str!("fixtures/generate_jitconfig_response.json");

/// The `client_id` inside the fixture envelope — `register` lifts it into
/// `credentials.json` after parsing the minted config.
const FIXTURE_CLIENT_ID: &str = "toolu-runner-fixture-client";

/// Base `toolu-runner` invocation: `TOOLU_RUNNER_HOME` and `HOME` pinned
/// to the fresh test home (no real user state, no gitconfig URL rewrites
/// in the child's `git` calls), ambient `TOOLU_RUNNER_TOKEN` scrubbed,
/// stdio piped — so stderr inside the child is never a TTY.
fn runner_cmd(home: &Path) -> std::process::Command {
  let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_toolu-runner"));
  cmd
    .env("TOOLU_RUNNER_HOME", home)
    .env("HOME", home)
    .env_remove("TOOLU_RUNNER_TOKEN")
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped());
  cmd
}

/// Mount the `generate-jitconfig` 201 for `<owner>/<repo>`, matching ONLY
/// requests carrying `Authorization: Bearer <bearer>` — the matcher plus
/// `expect(1)` (verified when the server drops) is the server-side proof
/// of which token the binary sent. `?` (not `expect`) keeps this
/// non-`#[test]` helper clippy-clean.
async fn mount_jitconfig(
  server: &MockServer,
  owner_repo: &str,
  bearer: &str,
) -> Result<(), serde_json::Error> {
  let body: serde_json::Value = serde_json::from_str(RESPONSE_FIXTURE)?;
  Mock::given(method("POST"))
    .and(request_path(format!(
      "/api/v3/repos/{owner_repo}/actions/runners/generate-jitconfig"
    )))
    .and(header("authorization", format!("Bearer {bearer}")))
    .respond_with(ResponseTemplate::new(201).set_body_json(body))
    .expect(1)
    .mount(server)
    .await;
  Ok(())
}

/// Run `git -C <cwd> <args>` and assert it succeeded.
fn run_git(cwd: &Path, args: &[&str]) -> Result<(), std::io::Error> {
  let output = std::process::Command::new("git")
    .arg("-C")
    .arg(cwd)
    .args(args)
    .output()?;
  assert!(
    output.status.success(),
    "git {args:?} failed: {}",
    String::from_utf8_lossy(&output.stderr)
  );
  Ok(())
}

/// `git init` a fresh tempdir and point its `origin` remote at `remote`.
fn git_repo_with_origin(remote: &str) -> Result<tempfile::TempDir, std::io::Error> {
  let dir = tempfile::tempdir()?;
  run_git(dir.path(), &["init", "--quiet"])?;
  run_git(dir.path(), &["remote", "add", "origin", remote])?;
  Ok(dir)
}

/// Hermeticity guard for the no-token test: the spawned binary reads the
/// same store `AuthStore::new` picks here. The `File` backend is always
/// clean (fresh tempdir home); a reachable OS keyring is machine-global,
/// so a real github.com login token could leak into the child and send a
/// real api.github.com request — report `false` so the test skips.
fn no_stored_dotcom_token(home: &Path) -> bool {
  let store = AuthStore::new(home);
  match &store {
    AuthStore::File(_) => true,
    AuthStore::Keyring => !matches!(store.load("github.com"), Ok(Some(_))),
  }
}

/// AC-3 / AC-12: explicit `--url` + `TOOLU_RUNNER_TOKEN` env → the state
/// lands in the per-repo `runners/<owner>/<repo>/` dir, both files 0600,
/// the persisted `data_dir` IS that per-repo dir, the workspace default
/// is unchanged, and `credentials.json` carries the client_id lifted from
/// the genuinely parsed JIT envelope.
#[tokio::test(flavor = "multi_thread")]
async fn register_persists_per_repo_layout_from_explicit_url() {
  let home = tempfile::tempdir().expect("home tempdir");
  let server = MockServer::start().await;
  mount_jitconfig(&server, "testowner/testrepo", "env-token-t1")
    .await
    .expect("mount fixture mock");

  let url = format!("{}/testowner/testrepo", server.uri());
  let output = runner_cmd(home.path())
    .env("TOOLU_RUNNER_TOKEN", "env-token-t1")
    .args(["register", "--url", &url, "--name", "t1-runner"])
    .output()
    .expect("spawn register");
  assert!(
    output.status.success(),
    "register against the fixture server must succeed; stderr: {}",
    String::from_utf8_lossy(&output.stderr)
  );

  assert_per_repo_state(home.path(), &url).expect("persisted per-repo state must be valid");
}

/// Assert the persisted per-repo state for `testowner/testrepo`: both
/// files present + 0600, the parsed `config.toml` carrying the fixture
/// registration with the per-repo `data_dir` and the unchanged workspace
/// default, and `credentials.json` carrying the envelope's client_id.
/// `?` (not `expect`) keeps this non-`#[test]` helper clippy-clean.
fn assert_per_repo_state(home: &Path, url: &str) -> Result<(), Box<dyn std::error::Error>> {
  let repo_dir = home.join("runners/testowner/testrepo");
  let config_path = repo_dir.join("config.toml");
  let creds_path = repo_dir.join("credentials.json");
  assert!(
    config_path.is_file(),
    "config.toml missing at {}",
    config_path.display()
  );
  assert!(
    creds_path.is_file(),
    "credentials.json missing at {}",
    creds_path.display()
  );

  let cfg = load_config(&config_path)?;
  assert_eq!(cfg.runner_id, 461, "fixture runner id persisted");
  assert_eq!(cfg.runner_url, url);
  assert_eq!(cfg.runner_name, "t1-runner");
  assert_eq!(
    cfg.runtime.data_dir,
    repo_dir.to_string_lossy().as_ref(),
    "data_dir must be the per-repo dir (AC-12)"
  );
  assert_eq!(
    cfg.runtime.work_dir, "~/.toolu-runner/_work",
    "workspace default unchanged (AC-12)"
  );
  assert!(
    !cfg.runtime.jit_config.is_empty(),
    "the minted JIT blob must be persisted"
  );

  let creds = load_credentials(&creds_path)?;
  assert_eq!(
    creds.access_token, FIXTURE_CLIENT_ID,
    "credentials carry the client_id lifted from the parsed envelope"
  );

  assert_secret_modes(&[&config_path, &creds_path])?;
  Ok(())
}

/// Assert every file is mode 0600 (owner read/write only). Unix-only:
/// elsewhere the writer is best-effort and there is nothing to assert.
#[cfg(unix)]
fn assert_secret_modes(files: &[&Path]) -> Result<(), std::io::Error> {
  use std::os::unix::fs::PermissionsExt;
  for file in files {
    let mode = std::fs::metadata(file)?.permissions().mode();
    assert_eq!(
      mode & 0o777,
      0o600,
      "{} must be 0600; got {:o}",
      file.display(),
      mode & 0o777
    );
  }
  Ok(())
}

/// Non-unix: file modes are best-effort, nothing to assert.
#[cfg(not(unix))]
fn assert_secret_modes(_files: &[&Path]) -> Result<(), std::io::Error> {
  Ok(())
}

/// AC-6-adjacent: inference succeeds (real github.com `origin`), but with
/// no token anywhere and no TTY the run must stop at the `decide_bearer`
/// Fail gate — exit non-zero, stderr naming all three manual options.
/// Zero network: the failure happens before any HTTP call.
#[test]
fn zero_arg_register_without_token_fails_listing_manual_options() {
  let home = tempfile::tempdir().expect("home tempdir");
  if !no_stored_dotcom_token(home.path()) {
    eprintln!(
      "skipping: the OS keyring holds a real github.com login token — not hermetic on this machine"
    );
    return;
  }
  let repo = git_repo_with_origin("https://github.com/o/r.git").expect("temp git repo");

  let output = runner_cmd(home.path())
    .current_dir(repo.path())
    .arg("register")
    .output()
    .expect("spawn register");

  assert!(
    !output.status.success(),
    "no token + no TTY must exit non-zero"
  );
  let stderr = String::from_utf8_lossy(&output.stderr);
  for needle in ["--token", "TOOLU_RUNNER_TOKEN", "login"] {
    assert!(
      stderr.contains(needle),
      "stderr must name {needle} (proves inference reached the token gate): {stderr}"
    );
  }
}

/// AC-4: a non-github.com `origin` remote errors naming GHES and the
/// `--url` escape hatch — inference is github.com only.
#[test]
fn zero_arg_register_rejects_non_github_origin() {
  let home = tempfile::tempdir().expect("home tempdir");
  let repo = git_repo_with_origin("https://ghes.example.com/o/r.git").expect("temp git repo");

  let output = runner_cmd(home.path())
    .current_dir(repo.path())
    .arg("register")
    .output()
    .expect("spawn register");

  assert!(
    !output.status.success(),
    "a non-github.com origin must exit non-zero"
  );
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(stderr.contains("--url"), "error must name --url: {stderr}");
  assert!(stderr.contains("GHES"), "error must name GHES: {stderr}");
  assert!(
    stderr.contains("ghes.example.com"),
    "error must name the offending host: {stderr}"
  );
}

/// AC-4: outside any git repository, inference errors naming `--url`.
#[test]
fn zero_arg_register_outside_git_repo_names_url() {
  let home = tempfile::tempdir().expect("home tempdir");
  let cwd = tempfile::tempdir().expect("cwd tempdir");

  let output = runner_cmd(home.path())
    .current_dir(cwd.path())
    .arg("register")
    .output()
    .expect("spawn register");

  assert!(
    !output.status.success(),
    "register outside a git repo must exit non-zero"
  );
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(stderr.contains("--url"), "error must name --url: {stderr}");
}

/// AC-6: ONE token in the HOME-ROOT store serves registrations for two
/// different repos — the store is per-host at the runner home, never
/// sharded per repo dir. Each fixture mock matches only `Authorization:
/// Bearer <stored>` (server-side proof, `expect(1)` verified on drop),
/// and both registers run with no `--token` flag and no env token.
#[tokio::test(flavor = "multi_thread")]
async fn stored_token_at_home_root_is_shared_across_repo_registrations() {
  let home = tempfile::tempdir().expect("home tempdir");
  let store = AuthStore::new(home.path());
  if matches!(store, AuthStore::Keyring) {
    eprintln!(
      "skipping: OS keyring reachable — seeding it would write the machine-global store \
       (runs fully on keyless environments, e.g. Linux CI)"
    );
    return;
  }
  store
    .save(&StoredToken {
      access_token: "gho_stored_shared".to_owned(),
      scope: "repo".to_owned(),
      host: "127.0.0.1".to_owned(),
      issued_at: "2026-07-12T00:00:00+00:00".to_owned(),
    })
    .expect("seed the home-root stored token");

  let server = MockServer::start().await;
  mount_jitconfig(&server, "o2/r2", "gho_stored_shared")
    .await
    .expect("mount o2/r2 mock");
  mount_jitconfig(&server, "o3/r3", "gho_stored_shared")
    .await
    .expect("mount o3/r3 mock");

  for (owner, repo) in [("o2", "r2"), ("o3", "r3")] {
    let url = format!("{}/{owner}/{repo}", server.uri());
    let output = runner_cmd(home.path())
      .args(["register", "--url", &url, "--name", "shared-store-runner"])
      .output()
      .expect("spawn register");
    assert!(
      output.status.success(),
      "register {owner}/{repo} with only the stored token must succeed; stderr: {}",
      String::from_utf8_lossy(&output.stderr)
    );
    let config_path = home
      .path()
      .join("runners")
      .join(owner)
      .join(repo)
      .join("config.toml");
    assert!(
      config_path.is_file(),
      "per-repo config for {owner}/{repo} missing at {}",
      config_path.display()
    );
  }
}
