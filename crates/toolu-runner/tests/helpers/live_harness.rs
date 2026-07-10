//! `LiveHarness` — shared helpers for the live test suite (step 12).
//!
//! Exercises the full CLI surface (`register`, `run --once`, `remove`)
//! end-to-end against a real test repo. Each test gets a fresh temp
//! `~/.toolu-runner` stand-in and a built `toolu-runner` binary.
//!
//! Included via `#[path = "helpers/live_harness.rs"] mod harness;` by
//! the live test entry `tests/live_e2e.rs` — Cargo does not compile
//! `tests/helpers/**` as a test target of its own. The GitHub REST helpers (workflow
//! push/delete, dispatch, run polling, teardown) live in the child
//! module [`api`] (`live_harness_api.rs`).
//!
//! ## Env contract
//!
//! - `TOOLU_RUNNER_LIVE_TOKEN` — GitHub classic PAT with `repo` +
//!   `workflow` scopes on the test repo. Classic `repo` includes
//!   repository administration, which `generate-jitconfig` requires;
//!   a fine-grained token needs explicit `administration:write`.
//! - `TOOLU_RUNNER_LIVE_REPO` — `owner/name` of the test repo.
//! - `TOOLU_RUNNER_LIVE_BRANCH` — optional, defaults to `main`.
//!
//! Read at runtime by `new()`. The harness compiles fine without
//! them; the `live` feature only gates compilation, not runtime.
//! Individual tests check the env vars up front and early-return.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tempfile::TempDir;
use tokio::process::{Child, Command};

#[path = "live_harness_api.rs"]
mod api;

/// User agent string sent to the GitHub API. Stable so request logs are
/// filterable in the test repo's audit trail.
const USER_AGENT: &str = "toolu-runner-live-tests";

/// Default branch assumed for workflow dispatch. Override via
/// `TOOLU_RUNNER_LIVE_BRANCH` if the test repo uses a different default.
fn dispatch_branch() -> String {
  std::env::var("TOOLU_RUNNER_LIVE_BRANCH").unwrap_or_else(|_| "main".to_owned())
}

/// Workspace root — `cargo build` runs from here.
fn workspace_root() -> PathBuf {
  let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
  // `cargo` always sets CARGO_MANIFEST_DIR to a path that has at
  // least one parent (the workspace root). Fall back to "." for
  // hand-rolled `cargo run` invocations.
  match manifest.parent() {
    Some(p) => p.to_path_buf(),
    None => PathBuf::from("."),
  }
}

/// Resolved path to the `toolu-runner` binary after `cargo build`.
fn binary_path() -> PathBuf {
  let target = std::env::var("CARGO_TARGET_DIR")
    .map(PathBuf::from)
    .unwrap_or_else(|_| workspace_root().join("target"));
  target.join("debug").join("toolu-runner")
}

/// Build the `toolu-runner` binary with the `live` feature. Returns
/// the resolved path on success.
async fn build_binary() -> Result<PathBuf, Box<dyn std::error::Error>> {
  let path = binary_path();
  let status = Command::new("cargo")
    .args([
      "build",
      "-p",
      "toolu-runner",
      "--features",
      "live",
      "--bin",
      "toolu-runner",
    ])
    .current_dir(workspace_root())
    .stdout(Stdio::null())
    .stderr(Stdio::inherit())
    .status()
    .await?;
  if !status.success() {
    return Err(format!("cargo build failed with status {status}").into());
  }
  Ok(path)
}

/// `LiveHarness` — owns the per-test config dir, the PAT, the target
/// repo, and the resolved binary path. Drop the value to remove the
/// temp config dir; call [`Self::cleanup`] to also tear down the
/// registered runner and pushed workflow files.
pub struct LiveHarness {
  /// Absolute path to the built `toolu-runner` binary.
  pub binary_path: PathBuf,
  /// Per-test stand-in for `~/.toolu-runner/`. Removed on drop.
  pub config_dir: TempDir,
  /// Per-test stand-in for `~/.toolu-runner/_work/`.
  pub work_dir: PathBuf,
  /// `owner/name` of the test repo.
  pub repo: String,
  /// Personal access token with `repo` + `workflow` scopes.
  pub token: String,
  /// Cached default branch for workflow dispatch.
  branch: String,
}

impl LiveHarness {
  /// Read env vars, allocate a temp config dir, build the binary.
  pub async fn new() -> Result<Self, Box<dyn std::error::Error>> {
    let token = std::env::var("TOOLU_RUNNER_LIVE_TOKEN")
      .map_err(|e| format!("TOOLU_RUNNER_LIVE_TOKEN: {e}"))?;
    let repo = std::env::var("TOOLU_RUNNER_LIVE_REPO")
      .map_err(|e| format!("TOOLU_RUNNER_LIVE_REPO: {e}"))?;
    let branch = dispatch_branch();

    let config_dir = tempfile::Builder::new()
      .prefix("toolu-runner-live-")
      .tempdir()?;
    let work_dir = config_dir.path().join("_work");
    std::fs::create_dir_all(&work_dir)?;

    let binary_path = build_binary().await?;

    Ok(Self {
      binary_path,
      config_dir,
      work_dir,
      repo,
      token,
      branch,
    })
  }

  /// Absolute path to the persisted `config.toml`.
  pub fn config_path(&self) -> PathBuf {
    self.config_dir.path().join("config.toml")
  }

  /// Absolute path to the persisted `credentials.json`.
  pub fn credentials_path(&self) -> PathBuf {
    self.config_dir.path().join("credentials.json")
  }

  /// Base URL for the GitHub REST API.
  fn api_base(&self) -> String {
    "https://api.github.com".to_owned()
  }

  /// Build a configured `reqwest::Client`. One client per harness so
  /// the connection pool is reused across the API calls a test makes.
  fn http(&self) -> Result<reqwest::Client, Box<dyn std::error::Error>> {
    Ok(
      reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(30))
        .build()?,
    )
  }
}

/// CLI process surface — drives the built `toolu-runner` binary.
impl LiveHarness {
  /// Run `toolu-runner register` against the test repo. The runner
  /// name and labels are deterministic so re-runs of the harness
  /// against the same repo are idempotent (`--replace` overwrites).
  ///
  /// The PAT is passed straight through as `--token`:
  /// `generate-jitconfig` is a plain REST endpoint that authenticates
  /// with a PAT / App token holding repository administration (the
  /// classic `repo` scope covers it; fine-grained tokens need
  /// `administration:write`) — the `registration-token` exchange is a
  /// different (non-JIT) flow and yields 401 here.
  pub async fn register(&self) -> Result<(), Box<dyn std::error::Error>> {
    let url = format!("https://github.com/{}", self.repo);
    let runner_name = format!(
      "toolu-runner-live-{}",
      self.repo.replace('/', "-").to_lowercase()
    );
    let config_path = self
      .config_path()
      .to_str()
      .ok_or("config path utf-8")?
      .to_owned();
    let work_path = self.work_dir.to_str().ok_or("work dir utf-8")?.to_owned();
    let status = Command::new(&self.binary_path)
      .args([
        "register",
        "--url",
        &url,
        "--token",
        &self.token,
        "--name",
        &runner_name,
        "--labels",
        "self-hosted,toolu-runner-v1,linux,x64",
        "--config",
        &config_path,
        "--work",
        &work_path,
        "--replace",
      ])
      .status()
      .await?;
    if !status.success() {
      return Err(format!("register failed: {status}").into());
    }
    Ok(())
  }

  /// Spawn `toolu-runner run --once` and return the child handle. The
  /// child holds the single-job `.lock` and exits after the first job
  /// completes (or fails). Caller is responsible for waiting on the
  /// child and asserting its exit code.
  pub async fn run_once(&self) -> Result<Child, Box<dyn std::error::Error>> {
    let config_path = self
      .config_path()
      .to_str()
      .ok_or("config path utf-8")?
      .to_owned();
    let child = Command::new(&self.binary_path)
      .args(["run", "--once", "--config", &config_path])
      .stdout(Stdio::piped())
      .stderr(Stdio::piped())
      .kill_on_drop(true)
      .spawn()?;
    Ok(child)
  }

  /// Run `toolu-runner remove` against the persisted config.
  /// No-op if the config file is already gone.
  pub async fn remove(&self) -> Result<(), Box<dyn std::error::Error>> {
    if !self.config_path().exists() {
      return Ok(());
    }
    let config_path = self
      .config_path()
      .to_str()
      .ok_or("config path utf-8")?
      .to_owned();
    let status = Command::new(&self.binary_path)
      .args(["remove", "--config", &config_path])
      .status()
      .await?;
    if !status.success() {
      return Err(format!("remove failed: {status}").into());
    }
    Ok(())
  }

  /// Remove the registered runner and delete any pushed workflow
  /// files. Called by tests as the final step so subsequent runs
  /// start from a clean GH state. Workflow file list is passed in
  /// because the test knows which files it pushed.
  pub async fn cleanup(&self, workflows: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
    let _ = self.remove().await;
    for name in workflows {
      let _ = self.delete_workflow(name).await;
    }
    Ok(())
  }
}
