//! Live two-job loop E2E for the always-online `run` command (s9, AC-1/AC-2).
//!
//! Both tests drive the real GitHub API: they `register` a JIT runner against
//! a live repo, spawn `toolu-runner run`, dispatch a `workflow_dispatch`
//! workflow, and watch the run to completion. They are `#[ignore]`'d so the
//! default `cargo test` skips them, and they self-skip (LOUD `eprintln!` +
//! `Ok(())`) unless the whole environment gate is present — so
//! `cargo test -p toolu-runner --test run_loop_live` is green with nothing set.
//!
//! Environment gate (ALL required, else the test prints why and returns Ok):
//! - `TOOLU_TEST_GH_LIVE=1` — opt in to the live path.
//! - `TOOLU_RUNNER_TOKEN` — a PAT / token with `administration:write` on the
//!   target repo (used both for `register` and for the loop's re-mint bearer).
//! - `TOOLU_TEST_GH_URL` — `https://github.com/<owner>/<repo>`, a repo that
//!   carries a `workflow_dispatch`-triggerable workflow.
//! - `TOOLU_TEST_GH_WORKFLOW` — the workflow file name (default `test.yml`).
//! - `TOOLU_TEST_GH_REF` — the git ref to dispatch against (default `main`).
//!
//! No mocks: the skip path is the only branch that does not touch GitHub.

use std::collections::HashSet;
use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// User-Agent for the GitHub REST calls (GitHub rejects UA-less requests).
const USER_AGENT: &str = "toolu-runner-run-loop-live-test";
/// Pinned GitHub REST API version.
const GH_API_VERSION: &str = "2022-11-28";
/// Delay between run-status polls.
const POLL_INTERVAL: Duration = Duration::from_secs(5);
/// Delay between child-exit polls.
const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(500);
/// Budget for the first dispatched job to reach `completed`.
const FIRST_JOB_TIMEOUT: Duration = Duration::from_secs(120);
/// Budget for the second job — it only lands after an automatic re-mint.
const REMINT_JOB_TIMEOUT: Duration = Duration::from_secs(180);
/// Budget for the loop to exit after SIGINT.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);
/// Budget for `run --once` to exit on its own after its single job.
const ONCE_EXIT_TIMEOUT: Duration = Duration::from_secs(30);

/// The resolved live-test environment.
struct GhLive {
  token: String,
  url: String,
  owner: String,
  repo: String,
  workflow: String,
  git_ref: String,
}

/// The subset of a GitHub Actions run we assert on.
struct Run {
  id: u64,
  status: String,
  conclusion: Option<String>,
}

/// Owns a spawned `run` child so a panicking assertion still reaps it (a
/// leaked `run` would keep polling GitHub and holding the temp registration).
struct ChildGuard(Child);

impl Drop for ChildGuard {
  fn drop(&mut self) {
    let _ = self.0.kill();
    let _ = self.0.wait();
  }
}

/// Read every required env var, returning a skip reason (`Err`) when the gate
/// is incomplete so the caller can `eprintln!` it and return `Ok`.
fn gh_env() -> Result<GhLive, String> {
  if std::env::var("TOOLU_TEST_GH_LIVE").ok().as_deref() != Some("1") {
    return Err("TOOLU_TEST_GH_LIVE is not set to 1".to_owned());
  }
  let token = require_env("TOOLU_RUNNER_TOKEN")?;
  let url = require_env("TOOLU_TEST_GH_URL")?;
  let (owner, repo) = parse_owner_repo(&url)?;
  let workflow = std::env::var("TOOLU_TEST_GH_WORKFLOW").unwrap_or_else(|_| "test.yml".to_owned());
  let git_ref = std::env::var("TOOLU_TEST_GH_REF").unwrap_or_else(|_| "main".to_owned());
  Ok(GhLive {
    token,
    url,
    owner,
    repo,
    workflow,
    git_ref,
  })
}

/// Fetch a required env var, naming it in the skip reason when it is unset.
fn require_env(key: &str) -> Result<String, String> {
  std::env::var(key).map_err(|err| format!("{key} unavailable: {err}"))
}

/// Split `https://github.com/<owner>/<repo>[.git][/]` into `(owner, repo)`.
fn parse_owner_repo(url: &str) -> Result<(String, String), String> {
  let trimmed = url.trim_end_matches('/');
  let trimmed = trimmed.strip_suffix(".git").unwrap_or(trimmed);
  let mut segments = trimmed.rsplitn(3, '/');
  let repo = segments.next().filter(|s| !s.is_empty());
  let owner = segments.next().filter(|s| !s.is_empty());
  let repo = repo.ok_or_else(|| format!("cannot parse a repo from URL `{url}`"))?;
  let owner = owner.ok_or_else(|| format!("cannot parse an owner from URL `{url}`"))?;
  Ok((owner.to_owned(), repo.to_owned()))
}

/// A registration name unique per invocation so re-runs and the two tests
/// never collide on the same registration against the shared test repo.
fn unique_runner_name(tag: &str) -> String {
  let nanos = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map(|d| d.as_nanos())
    .unwrap_or_default();
  format!("toolu-live-{tag}-{nanos}")
}

/// Register a JIT runner under a temp home and return the persisted config
/// path (`<home>/runners/<owner>/<repo>/config.toml`). `--replace` keeps a
/// stale registration from failing a re-run; the token rides in the child env.
fn register_runner(home: &Path, gh: &GhLive, name: &str) -> Result<PathBuf, Box<dyn Error>> {
  let output = Command::new(env!("CARGO_BIN_EXE_toolu-runner"))
    .env("HOME", home)
    .env("TOOLU_RUNNER_HOME", home)
    .env("TOOLU_RUNNER_NO_KEYRING", "1")
    .env("TOOLU_RUNNER_TOKEN", &gh.token)
    .args([
      "register",
      "--url",
      gh.url.as_str(),
      "--name",
      name,
      "--replace",
    ])
    .output()?;
  if !output.status.success() {
    return Err(
      format!(
        "`register` failed ({}):\n--stdout--\n{}\n--stderr--\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
      )
      .into(),
    );
  }
  let config_path = home
    .join("runners")
    .join(&gh.owner)
    .join(&gh.repo)
    .join("config.toml");
  if !config_path.is_file() {
    return Err(format!("`register` wrote no config at {}", config_path.display()).into());
  }
  Ok(config_path)
}

/// Spawn `toolu-runner run [--once] --config <path>` with the child stderr
/// redirected to `stderr_path` for post-mortem diagnostics. The re-mint bearer
/// rides in `TOOLU_RUNNER_TOKEN` exactly as an operator would supply it.
fn spawn_run(
  home: &Path,
  config_path: &Path,
  gh: &GhLive,
  once: bool,
  stderr_path: &Path,
) -> Result<Child, Box<dyn Error>> {
  let stderr = std::fs::File::create(stderr_path)?;
  let mut cmd = Command::new(env!("CARGO_BIN_EXE_toolu-runner"));
  cmd
    .env("HOME", home)
    .env("TOOLU_RUNNER_HOME", home)
    .env("TOOLU_RUNNER_NO_KEYRING", "1")
    .env("TOOLU_RUNNER_TOKEN", &gh.token)
    .args(["run", "--config"])
    .arg(config_path)
    .stdout(Stdio::null())
    .stderr(Stdio::from(stderr));
  if once {
    cmd.arg("--once");
  }
  Ok(cmd.spawn()?)
}

/// Deliver SIGINT to the child by pid — no `unsafe`, just `kill -INT`.
/// Non-unix hosts get a clear error instead of a missing-binary failure.
#[cfg(unix)]
fn send_sigint(pid: u32) -> Result<(), Box<dyn Error>> {
  let status = Command::new("kill")
    .arg("-INT")
    .arg(pid.to_string())
    .status()?;
  if !status.success() {
    return Err(format!("`kill -INT {pid}` exited with {status}").into());
  }
  Ok(())
}

/// Non-unix stub: the graceful-shutdown assertion needs SIGINT.
#[cfg(not(unix))]
fn send_sigint(_pid: u32) -> Result<(), Box<dyn Error>> {
  Err("SIGINT delivery is not supported on this platform".into())
}

/// Best-effort read of the child's captured stderr for a failure message.
fn read_log(path: &Path) -> String {
  std::fs::read_to_string(path).unwrap_or_else(|err| format!("<no captured stderr: {err}>"))
}

/// Wrap a failure `context` with the child's captured stderr for diagnosis.
fn with_stderr(context: &str, stderr_path: &Path) -> Box<dyn Error> {
  let mut log = read_log(stderr_path);
  if log.is_empty() {
    log = "(no stderr captured — the failure likely happened during test setup)".to_owned();
  }
  format!("{context}\n--- captured run stderr ---\n{log}").into()
}

/// Attach the common GitHub REST headers (auth, media type, API version).
fn with_gh_headers(builder: reqwest::RequestBuilder, token: &str) -> reqwest::RequestBuilder {
  builder
    .bearer_auth(token)
    .header("Accept", "application/vnd.github+json")
    .header("X-GitHub-Api-Version", GH_API_VERSION)
}

/// POST a `workflow_dispatch` for the configured workflow + ref (expects 204).
async fn dispatch(client: &reqwest::Client, gh: &GhLive) -> Result<(), Box<dyn Error>> {
  let url = format!(
    "https://api.github.com/repos/{}/{}/actions/workflows/{}/dispatches",
    gh.owner, gh.repo, gh.workflow
  );
  let body = serde_json::json!({ "ref": gh.git_ref });
  let resp = with_gh_headers(client.post(&url), &gh.token)
    .json(&body)
    .send()
    .await?;
  let status = resp.status();
  if status != reqwest::StatusCode::NO_CONTENT {
    let detail = resp
      .text()
      .await
      .unwrap_or_else(|err| format!("<body read error: {err}>"));
    return Err(format!("workflow_dispatch returned {status} (expected 204): {detail}").into());
  }
  Ok(())
}

/// Deserialize one run entry from the list response.
fn parse_run(entry: &serde_json::Value) -> Result<Run, Box<dyn Error>> {
  let id = entry
    .get("id")
    .and_then(serde_json::Value::as_u64)
    .ok_or_else(|| format!("run entry missing a numeric `id`: {entry}"))?;
  let status = entry
    .get("status")
    .and_then(serde_json::Value::as_str)
    .ok_or_else(|| format!("run entry missing a string `status`: {entry}"))?
    .to_owned();
  let conclusion = entry
    .get("conclusion")
    .and_then(serde_json::Value::as_str)
    .map(str::to_owned);
  Ok(Run {
    id,
    status,
    conclusion,
  })
}

/// List recent `workflow_dispatch` runs for the repo (newest first).
async fn list_dispatch_runs(
  client: &reqwest::Client,
  gh: &GhLive,
) -> Result<Vec<Run>, Box<dyn Error>> {
  let url = format!(
    "https://api.github.com/repos/{}/{}/actions/runs?event=workflow_dispatch&per_page=30",
    gh.owner, gh.repo
  );
  let resp = with_gh_headers(client.get(&url), &gh.token).send().await?;
  let status = resp.status();
  let text = resp.text().await?;
  if !status.is_success() {
    return Err(format!("list runs returned {status}: {text}").into());
  }
  let payload: serde_json::Value = serde_json::from_str(&text)?;
  let entries = payload
    .get("workflow_runs")
    .and_then(serde_json::Value::as_array)
    .ok_or("runs response missing a `workflow_runs` array")?;
  entries.iter().map(parse_run).collect()
}

/// Snapshot the current `workflow_dispatch` run ids — the runs to exclude so a
/// later poll picks up only the run our dispatch creates.
async fn snapshot_ids(
  client: &reqwest::Client,
  gh: &GhLive,
) -> Result<HashSet<u64>, Box<dyn Error>> {
  Ok(
    list_dispatch_runs(client, gh)
      .await?
      .into_iter()
      .map(|run| run.id)
      .collect(),
  )
}

/// Poll until the newest run whose id is not in `exclude` reaches `completed`,
/// or `timeout` elapses. Run ids are monotonic, so the max id is the newest.
async fn wait_for_completed(
  client: &reqwest::Client,
  gh: &GhLive,
  exclude: &HashSet<u64>,
  timeout: Duration,
  label: &str,
) -> Result<Run, Box<dyn Error>> {
  let deadline = Instant::now() + timeout;
  loop {
    let newest = list_dispatch_runs(client, gh)
      .await?
      .into_iter()
      .filter(|run| !exclude.contains(&run.id))
      .max_by_key(|run| run.id);
    if let Some(run) = newest
      && run.status == "completed"
    {
      return Ok(run);
    }
    if Instant::now() >= deadline {
      return Err(format!("timed out after {timeout:?} waiting for {label} to complete").into());
    }
    tokio::time::sleep(POLL_INTERVAL).await;
  }
}

/// Poll `try_wait` until the child exits or `timeout` elapses (`None` = still
/// running at the deadline).
async fn wait_for_exit(
  child: &mut Child,
  timeout: Duration,
) -> Result<Option<ExitStatus>, Box<dyn Error>> {
  let deadline = Instant::now() + timeout;
  loop {
    if let Some(status) = child.try_wait()? {
      return Ok(Some(status));
    }
    if Instant::now() >= deadline {
      return Ok(None);
    }
    tokio::time::sleep(EXIT_POLL_INTERVAL).await;
  }
}

/// Resolve the environment gate, or print a LOUD skip reason and return `None`
/// so the caller returns `Ok` — keeps the ungated `cargo test` green.
fn resolve_or_skip(test: &str, ac: &str) -> Option<GhLive> {
  match gh_env() {
    Ok(gh) => Some(gh),
    Err(reason) => {
      eprintln!("SKIP run_loop_live::{test}: {reason} — {ac} NOT covered");
      None
    },
  }
}

/// Snapshot existing runs, dispatch a fresh one, wait for it to complete, and
/// assert (tightly) that it concluded `success` — failures carry the child's
/// stderr. Sequential calls naturally exclude the prior job's run.
async fn dispatch_job(
  client: &reqwest::Client,
  gh: &GhLive,
  timeout: Duration,
  label: &str,
  stderr_path: &Path,
) -> Result<Run, Box<dyn Error>> {
  let exclude = snapshot_ids(client, gh).await?;
  dispatch(client, gh).await?;
  let run = wait_for_completed(client, gh, &exclude, timeout, label)
    .await
    .map_err(|err| with_stderr(&err.to_string(), stderr_path))?;
  if run.conclusion.as_deref() != Some("success") {
    return Err(with_stderr(
      &format!("{label} must conclude `success`; got {:?}", run.conclusion),
      stderr_path,
    ));
  }
  Ok(run)
}

/// AC-1: one `run` invocation (no `--once`) executes two successively
/// dispatched jobs — the second only after an automatic JIT re-mint — then
/// exits cleanly on SIGINT.
#[tokio::test]
#[ignore = "live GitHub test — requires TOOLU_TEST_GH_LIVE + TOOLU_RUNNER_TOKEN + TOOLU_TEST_GH_URL"]
async fn two_jobs_one_invocation() -> Result<(), Box<dyn Error>> {
  let Some(gh) = resolve_or_skip(
    "two_jobs_one_invocation",
    "AC-1 (always-online two-job loop)",
  ) else {
    return Ok(());
  };

  let client = reqwest::Client::builder().user_agent(USER_AGENT).build()?;
  let home = tempfile::tempdir()?;
  let config_path = register_runner(home.path(), &gh, &unique_runner_name("loop"))?;
  let stderr_path = home.path().join("run.stderr.log");
  let mut guard = ChildGuard(spawn_run(
    home.path(),
    &config_path,
    &gh,
    false,
    &stderr_path,
  )?);

  dispatch_job(&client, &gh, FIRST_JOB_TIMEOUT, "job 1", &stderr_path).await?;

  // The loop must NOT exit after the first job (always-online default).
  assert!(
    guard.0.try_wait()?.is_none(),
    "`run` exited after job 1 instead of staying online.\n{}",
    read_log(&stderr_path)
  );

  // Job 2 lands only after an automatic re-mint — no manual re-register.
  dispatch_job(
    &client,
    &gh,
    REMINT_JOB_TIMEOUT,
    "job 2 (post re-mint)",
    &stderr_path,
  )
  .await?;

  shutdown_cleanly(&mut guard, &stderr_path).await
}

/// SIGINT the child and require a clean (status 0) exit within
/// [`SHUTDOWN_TIMEOUT`] — the spec's "cancelled → exit 0" contract.
async fn shutdown_cleanly(
  guard: &mut ChildGuard,
  stderr_path: &Path,
) -> Result<(), Box<dyn Error>> {
  send_sigint(guard.0.id())?;
  let status = wait_for_exit(&mut guard.0, SHUTDOWN_TIMEOUT)
    .await?
    .ok_or_else(|| {
      with_stderr(
        &format!("`run` did not exit within {SHUTDOWN_TIMEOUT:?} of SIGINT"),
        stderr_path,
      )
    })?;
  if !status.success() {
    return Err(with_stderr(
      &format!("`run` must exit cleanly on SIGINT; got {status}"),
      stderr_path,
    ));
  }
  Ok(())
}

/// AC-2: `run --once` executes exactly one job and then exits by itself with
/// the listener's exit status — no signal sent.
#[tokio::test]
#[ignore = "live GitHub test — requires TOOLU_TEST_GH_LIVE + TOOLU_RUNNER_TOKEN + TOOLU_TEST_GH_URL"]
async fn once_exits_after_first_job() -> Result<(), Box<dyn Error>> {
  let Some(gh) = resolve_or_skip(
    "once_exits_after_first_job",
    "AC-2 (--once single-job exit)",
  ) else {
    return Ok(());
  };

  let client = reqwest::Client::builder().user_agent(USER_AGENT).build()?;
  let home = tempfile::tempdir()?;
  let config_path = register_runner(home.path(), &gh, &unique_runner_name("once"))?;
  let stderr_path = home.path().join("run.stderr.log");
  let mut guard = ChildGuard(spawn_run(
    home.path(),
    &config_path,
    &gh,
    true,
    &stderr_path,
  )?);

  dispatch_job(
    &client,
    &gh,
    FIRST_JOB_TIMEOUT,
    "the --once job",
    &stderr_path,
  )
  .await?;

  // `--once` must exit on its own after the single job — no signal sent.
  let status = wait_for_exit(&mut guard.0, ONCE_EXIT_TIMEOUT)
    .await?
    .ok_or_else(|| {
      with_stderr(
        &format!("`run --once` did not exit within {ONCE_EXIT_TIMEOUT:?} of job completion"),
        &stderr_path,
      )
    })?;
  assert!(
    status.success(),
    "`run --once` must exit 0 after its single job; got {status}.\n{}",
    read_log(&stderr_path)
  );
  Ok(())
}
