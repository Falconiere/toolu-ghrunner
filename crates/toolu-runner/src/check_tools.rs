//! `check` tool probes: git, Docker and Node availability from the real
//! binaries on `PATH`. Every tool is optional — a missing or broken one is a
//! warning naming what it limits, never a failure. Each subprocess is bounded
//! by the probe timeout and killed when it overruns.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

use crate::check_env::redact_url;
use crate::config;

/// The daemon endpoint bollard and the docker CLI use without `DOCKER_HOST`.
const DEFAULT_DOCKER_ENDPOINT: &str = "unix:///var/run/docker.sock";

/// How a tool probe turned out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolStatus {
  /// Present and working.
  Ok,
  /// Missing, broken or partly available.
  Warn,
}

/// One tool line in the report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToolReport {
  /// `git`, `docker` or `node`.
  pub(crate) name: &'static str,
  /// `Ok` or `Warn`.
  pub(crate) status: ToolStatus,
  /// The version or the problem, plus context.
  pub(crate) summary: String,
}

/// Why running a tool did not produce output.
enum RunError {
  /// Not on `PATH`.
  NotFound,
  /// Still running at the bound; killed.
  TimedOut,
  /// Could not be started for another reason.
  Spawn(String),
}

/// Captured output of a tool that exited (successfully or not).
struct Ran {
  success: bool,
  stdout: String,
  stderr: String,
}

/// Run `program args…` with stdin closed, killing it after `timeout`.
async fn run(program: &str, args: &[&str], timeout: Duration) -> Result<Ran, RunError> {
  let child = Command::new(program)
    .args(args)
    .stdin(Stdio::null())
    .kill_on_drop(true)
    .output();
  match tokio::time::timeout(timeout, child).await {
    Err(_elapsed) => Err(RunError::TimedOut),
    Ok(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => Err(RunError::NotFound),
    Ok(Err(error)) => Err(RunError::Spawn(error.to_string())),
    Ok(Ok(output)) => Ok(Ran {
      success: output.status.success(),
      stdout: String::from_utf8_lossy(&output.stdout).trim().to_owned(),
      stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    }),
  }
}

fn warn(name: &'static str, summary: String) -> ToolReport {
  ToolReport {
    name,
    status: ToolStatus::Warn,
    summary,
  }
}

fn first_line(text: &str) -> &str {
  text.lines().next().unwrap_or_default()
}

/// Turn a failed run into a warning with `missing` explaining the impact.
fn run_failure(
  name: &'static str,
  error: RunError,
  missing: &str,
  timeout: Duration,
) -> ToolReport {
  match error {
    RunError::NotFound => warn(name, format!("not found on PATH ({missing})")),
    RunError::TimedOut => warn(
      name,
      format!("timed out after {} s and was killed", timeout.as_secs()),
    ),
    RunError::Spawn(error) => warn(name, format!("could not start: {error}")),
  }
}

/// A simple `<tool> --version` probe.
async fn version_probe(name: &'static str, missing: &str, timeout: Duration) -> ToolReport {
  match run(name, &["--version"], timeout).await {
    Ok(ran) if ran.success => ToolReport {
      name,
      status: ToolStatus::Ok,
      summary: first_line(&ran.stdout).to_owned(),
    },
    Ok(ran) => warn(
      name,
      format!("`{name} --version` failed: {}", first_line(&ran.stderr)),
    ),
    Err(error) => run_failure(name, error, missing, timeout),
  }
}

/// `git --version`.
pub(crate) async fn probe_git(timeout: Duration) -> ToolReport {
  version_probe(
    "git",
    "actions/checkout falls back to the REST API tarball; `run:` steps calling git fail",
    timeout,
  )
  .await
}

/// `node --version`, plus the Node runtimes the runner has cached under
/// `<data_dir>/node/` when a registration resolved.
pub(crate) async fn probe_node(timeout: Duration, data_dir: Option<&Path>) -> ToolReport {
  let mut report = version_probe(
    "node",
    "optional: the runner downloads Node for JavaScript actions",
    timeout,
  )
  .await;
  if let Some(dir) = data_dir {
    // `read_dir` blocks; keep it off the async workers.
    let dir = dir.to_path_buf();
    let cached = tokio::task::spawn_blocking(move || cached_runtimes(&dir))
      .await
      .unwrap_or_else(|error| format!("unknown ({error})"));
    report.summary = format!("{}  cached runtimes: {cached}", report.summary);
  }
  report
}

/// Directory names under `<data_dir>/node/`, sorted, or `none`.
fn cached_runtimes(data_dir: &Path) -> String {
  let mut names: Vec<String> = std::fs::read_dir(data_dir.join("node"))
    .map(|entries| {
      entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect()
    })
    .unwrap_or_default();
  names.sort();
  if names.is_empty() {
    "none".to_owned()
  } else {
    names.join(", ")
  }
}

/// The daemon endpoint the runner resolves, redacted for printing.
pub(crate) fn docker_endpoint() -> String {
  match config::docker_host().filter(|host| !host.is_empty()) {
    Some(host) => redact_url(&host),
    None => format!("{DEFAULT_DOCKER_ENDPOINT} (default)"),
  }
}

/// `docker version` client + server versions and the runner's endpoint.
pub(crate) async fn probe_docker(timeout: Duration) -> ToolReport {
  let format = "{{.Client.Version}}|{{.Server.Version}}";
  let endpoint = docker_endpoint();
  let linux_only = if cfg!(target_os = "linux") {
    ""
  } else {
    "  (container jobs and Docker actions are Linux-only)"
  };
  let missing = "container jobs, service containers and Docker actions are unavailable";
  let mut report = match run("docker", &["version", "--format", format], timeout).await {
    Ok(ran) if ran.success && !ran.stdout.contains('|') => warn(
      "docker",
      format!(
        "unexpected `docker version` output: {}",
        first_line(&ran.stdout)
      ),
    ),
    Ok(ran) => {
      let (client, server) = ran
        .stdout
        .split_once('|')
        .unwrap_or((ran.stdout.as_str(), ""));
      if ran.success && !server.is_empty() {
        ToolReport {
          name: "docker",
          status: ToolStatus::Ok,
          summary: format!("CLI {client}, daemon {server}"),
        }
      } else {
        let client = if client.is_empty() { "?" } else { client };
        warn(
          "docker",
          format!(
            "CLI {client}; daemon unreachable: {}",
            first_line(&ran.stderr)
          ),
        )
      }
    },
    Err(error) => run_failure("docker", error, missing, timeout),
  };
  report.summary = format!("{}  endpoint: {endpoint}{linux_only}", report.summary);
  report
}
