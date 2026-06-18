//! `LiveHarness` — shared helpers for the live test suite (step 12).
//!
//! Exercises the full CLI surface (`register`, `run --once`, `remove`)
//! end-to-end against a real test repo. Each test gets a fresh temp
//! `~/.toolu-runner` stand-in and a built `toolu-runner` binary.
//!
//! ## Env contract
//!
//! - `TOOLU_RUNNER_LIVE_TOKEN` — GitHub PAT with `repo` + `workflow`
//!   scopes on the test repo.
//! - `TOOLU_RUNNER_LIVE_REPO` — `owner/name` of the test repo.
//! - `TOOLU_RUNNER_LIVE_BRANCH` — optional, defaults to `main`.
//!
//! Read at runtime by `new()`. The harness compiles fine without
//! them; the `live` feature only gates compilation, not runtime.
//! Individual tests check the env vars up front and early-return.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use base64::Engine;
use tempfile::TempDir;
use tokio::process::{Child, Command};

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

  /// POST `/repos/{owner}/{repo}/actions/runners/registration-token`.
  /// Returns the short-lived token GH issues for one-time runner
  /// registration. The token expires in ~1h.
  pub async fn fetch_registration_token(&self) -> Result<String, Box<dyn std::error::Error>> {
    let url = format!(
      "{}/repos/{}/actions/runners/registration-token",
      self.api_base(),
      self.repo,
    );
    let client = self.http()?;
    let resp = client
      .post(&url)
      .bearer_auth(&self.token)
      .header("Accept", "application/vnd.github+json")
      .send()
      .await?;
    let status = resp.status();
    let body: serde_json::Value = resp.json().await?;
    if !status.is_success() {
      return Err(format!("registration-token POST failed: {status} {body}").into());
    }
    let token = body
      .get("token")
      .and_then(serde_json::Value::as_str)
      .ok_or("registration-token response missing `token` field")?
      .to_owned();
    Ok(token)
  }

  /// Run `toolu-runner register` against the test repo. The runner
  /// name and labels are deterministic so re-runs of the harness
  /// against the same repo are idempotent (`--replace` overwrites).
  pub async fn register(&self) -> Result<(), Box<dyn std::error::Error>> {
    let reg_token = self.fetch_registration_token().await?;
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
        &reg_token,
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

  /// PUT a workflow YAML to `.github/workflows/{name}` in the test
  /// repo. If the file already exists (re-run), the existing blob's
  /// `sha` is included in the PUT body so GH treats it as an update
  /// rather than a 422 conflict.
  pub async fn push_workflow(
    &self,
    name: &str,
    content: &str,
  ) -> Result<(), Box<dyn std::error::Error>> {
    let url = format!(
      "{}/repos/{}/contents/.github/workflows/{}",
      self.api_base(),
      self.repo,
      name,
    );
    let client = self.http()?;

    let existing_sha: Option<String> = client
      .get(&url)
      .bearer_auth(&self.token)
      .header("Accept", "application/vnd.github+json")
      .send()
      .await?
      .json::<serde_json::Value>()
      .await
      .ok()
      .and_then(|v| {
        v.get("sha")
          .and_then(serde_json::Value::as_str)
          .map(str::to_owned)
      });

    let encoded = base64::engine::general_purpose::STANDARD.encode(content.as_bytes());
    let mut body = serde_json::json!({
      "message": format!("test: push {name}"),
      "content": encoded,
    });
    if let Some(sha) = existing_sha {
      let map = body.as_object_mut().ok_or("body is not an object")?;
      map.insert("sha".to_owned(), serde_json::Value::String(sha));
    }

    let resp = client
      .put(&url)
      .bearer_auth(&self.token)
      .header("Accept", "application/vnd.github+json")
      .json(&body)
      .send()
      .await?;
    let status = resp.status();
    if !status.is_success() {
      let body: serde_json::Value = resp.json().await?;
      return Err(format!("workflow PUT failed: {status} {body}").into());
    }
    Ok(())
  }

  /// Delete a workflow file from the test repo. Best-effort —
  /// returns `Ok` on 404 (file already gone).
  pub async fn delete_workflow(&self, name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let url = format!(
      "{}/repos/{}/contents/.github/workflows/{}",
      self.api_base(),
      self.repo,
      name,
    );
    let client = self.http()?;
    let resp = client
      .get(&url)
      .bearer_auth(&self.token)
      .header("Accept", "application/vnd.github+json")
      .send()
      .await?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
      return Ok(());
    }
    let body: serde_json::Value = resp.json().await?;
    let Some(sha) = body
      .get("sha")
      .and_then(serde_json::Value::as_str)
      .map(str::to_owned)
    else {
      return Ok(());
    };
    let resp = client
      .delete(&url)
      .bearer_auth(&self.token)
      .header("Accept", "application/vnd.github+json")
      .json(&serde_json::json!({
        "message": format!("test: delete {name}"),
        "sha": sha,
      }))
      .send()
      .await?;
    let status = resp.status();
    if !status.is_success() {
      let body: serde_json::Value = resp.json().await?;
      return Err(format!("workflow DELETE failed: {status} {body}").into());
    }
    Ok(())
  }

  /// POST `/repos/{owner}/{repo}/actions/workflows/{name}/dispatches`
  /// to trigger `name` on the default branch. Returns the run id of
  /// the newly created run (GH returns 204 from dispatch; we list
  /// recent runs to pick the latest one).
  pub async fn trigger_workflow(&self, name: &str) -> Result<u64, Box<dyn std::error::Error>> {
    let dispatch_url = format!(
      "{}/repos/{}/actions/workflows/{}/dispatches",
      self.api_base(),
      self.repo,
      name,
    );
    let client = self.http()?;
    let resp = client
      .post(&dispatch_url)
      .bearer_auth(&self.token)
      .header("Accept", "application/vnd.github+json")
      .json(&serde_json::json!({"ref": self.branch}))
      .send()
      .await?;
    let status = resp.status();
    if !status.is_success() {
      let body: serde_json::Value = resp.json().await.unwrap_or_default();
      return Err(format!("workflow dispatch failed: {status} {body}").into());
    }

    let list_url = format!(
      "{}/repos/{}/actions/workflows/{}/runs?per_page=1",
      self.api_base(),
      self.repo,
      name,
    );
    for _ in 0..20 {
      tokio::time::sleep(Duration::from_secs(2)).await;
      let runs: serde_json::Value = client
        .get(&list_url)
        .bearer_auth(&self.token)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?
        .json()
        .await?;
      let id = runs
        .get("workflow_runs")
        .and_then(serde_json::Value::as_array)
        .and_then(|arr| arr.first())
        .and_then(|v| v.get("id"))
        .and_then(serde_json::Value::as_u64);
      if let Some(id) = id {
        return Ok(id);
      }
    }
    Err(
      format!(
        "could not find a run for {name} after dispatching on {}",
        self.branch
      )
      .into(),
    )
  }

  /// Poll `GET /repos/{owner}/{repo}/actions/runs/{run_id}` until the
  /// run's `status` flips to `completed`. Returns the `conclusion`
  /// field — `success`, `failure`, `cancelled`, or `skipped`.
  pub async fn wait_for_run(
    &self,
    run_id: u64,
    timeout: Duration,
  ) -> Result<String, Box<dyn std::error::Error>> {
    let url = format!(
      "{}/repos/{}/actions/runs/{}",
      self.api_base(),
      self.repo,
      run_id,
    );
    let client = self.http()?;
    let deadline = Instant::now() + timeout;
    loop {
      let run: serde_json::Value = client
        .get(&url)
        .bearer_auth(&self.token)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?
        .json()
        .await?;
      let status = run
        .get("status")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
      if status == "completed" {
        let conclusion = run
          .get("conclusion")
          .and_then(serde_json::Value::as_str)
          .unwrap_or("unknown")
          .to_owned();
        return Ok(conclusion);
      }
      if Instant::now() >= deadline {
        return Err(format!("run {run_id} did not complete within {timeout:?}").into());
      }
      tokio::time::sleep(Duration::from_secs(5)).await;
    }
  }

  /// Cancel a run via
  /// `POST /repos/{owner}/{repo}/actions/runs/{run_id}/cancel`.
  /// Used by the AC #14 test to verify the runner reacts to GH-side
  /// cancellation.
  pub async fn cancel_run(&self, run_id: u64) -> Result<(), Box<dyn std::error::Error>> {
    let url = format!(
      "{}/repos/{}/actions/runs/{}/cancel",
      self.api_base(),
      self.repo,
      run_id,
    );
    let client = self.http()?;
    let resp = client
      .post(&url)
      .bearer_auth(&self.token)
      .header("Accept", "application/vnd.github+json")
      .send()
      .await?;
    let status = resp.status();
    if !status.is_success() {
      let body: serde_json::Value = resp.json().await.unwrap_or_default();
      return Err(format!("run cancel POST failed: {status} {body}").into());
    }
    Ok(())
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
