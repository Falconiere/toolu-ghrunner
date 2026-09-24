//! Bounded GitHub.com verification for issue 73 job containers.
//!
//! This deliberately uses only the `gh` and `docker` CLIs: it does not build,
//! register, start, or authenticate a runner. Before selecting this ignored
//! test, provision two distinct dedicated runners whose labels are supplied by
//! `TOOLU_CONTAINER_TOOLU_LABEL` and `TOOLU_CONTAINER_REFERENCE_LABEL`.
//! The test must run where its Docker daemon owns those runner containers.
//! It cannot capture or sanitize acquired runner messages, and it does not
//! authenticate an official runner build; those are separate validation
//! surfaces. GHES is also outside this GitHub.com-only check.

#![cfg(feature = "live")]

mod job_container_live_command;

use job_container_live_command::{Captured, capture};
use std::collections::BTreeSet;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

const REPOSITORY: &str = "Falconiere/toolu-ghrunner";
const BRANCH: &str = "feat/73-run-jobs-inside-container-job";
const WORKFLOW: &str = "multistep-live.yml";
const RUN_PREFIX: &str = "job-container-73 / ";
const TIMEOUT: Duration = Duration::from_secs(15 * 60);
const POLL_INTERVAL: Duration = Duration::from_secs(5);
const ARTIFACT_NAME: &str = "container-73";
const ARTIFACT_BYTES: &[u8] = b"container-73-artifact\n";
const MARKERS: &[&str] = &[
  "CONTAINER_73_SHELL_OK",
  "CONTAINER_73_NODE_PRE_OK",
  "CONTAINER_73_NODE_MAIN_OK",
  "CONTAINER_73_NODE_POST_OK",
  "CONTAINER_73_COMPOSITE_OK",
  "CONTAINER_73_COMMAND_FILES_OK",
];

/// A run discovered after this test's dispatch. On unwinding, cancel only an
/// owned run that has not reached a terminal state; it never touches another
/// test's run, runner, container, network, or Docker resources.
struct OwnedRunGuard {
  unfinished: BTreeSet<u64>,
}

impl OwnedRunGuard {
  fn add(&mut self, run_id: u64) {
    self.unfinished.insert(run_id);
  }

  fn completed(&mut self, run_id: u64) {
    self.unfinished.remove(&run_id);
  }
}

impl Drop for OwnedRunGuard {
  fn drop(&mut self) {
    for run_id in &self.unfinished {
      let should_cancel = gh_capture(&[
        "run".to_owned(),
        "view".to_owned(),
        run_id.to_string(),
        "--repo".to_owned(),
        REPOSITORY.to_owned(),
        "--json".to_owned(),
        "status".to_owned(),
      ])
      .ok()
      .and_then(|output| {
        output
          .status
          .success()
          .then(|| json_string(&output.stdout, "status").ok())
          .flatten()
      })
      .is_some_and(|status| status != "completed");
      if should_cancel {
        let _ = gh_capture(&[
          "run".to_owned(),
          "cancel".to_owned(),
          run_id.to_string(),
          "--repo".to_owned(),
          REPOSITORY.to_owned(),
        ]);
      }
    }
  }
}

/// Terminal state returned by GitHub's workflow-run API through `gh`.
struct RunState {
  conclusion: String,
  status: String,
}

/// A per-lane artifact whose runtime file identifies the job container and
/// network that must be gone after GitHub has completed cleanup.
struct LaneResult {
  artifact_bytes: Vec<u8>,
  container_id: String,
  network: String,
}

/// Issue 73: both dedicated lanes execute the job-container workflow at the
/// same fixed branch revision, emit every container marker, upload identical
/// bytes, and leave their exact container and network absent from Docker.
#[test]
#[ignore = "live GitHub.com test — requires GH_TOKEN/GITHUB_TOKEN, docker, and two dedicated toolu-73-* runners"]
fn job_container_matches_reference_runner() -> Result<(), Box<dyn Error>> {
  require_token_env()?;
  require_cli(
    "gh",
    &["auth", "status", "--active", "--hostname", "github.com"],
  )?;
  require_cli("docker", &["info"])?;

  let toolu_label = required_label("TOOLU_CONTAINER_TOOLU_LABEL")?;
  let reference_label = required_label("TOOLU_CONTAINER_REFERENCE_LABEL")?;
  if toolu_label == reference_label {
    return Err(
      "TOOLU_CONTAINER_TOOLU_LABEL and TOOLU_CONTAINER_REFERENCE_LABEL must be distinct".into(),
    );
  }

  let workflow_sha = branch_sha()?;
  let deadline = Instant::now() + TIMEOUT;
  let mut guard = OwnedRunGuard {
    unfinished: BTreeSet::new(),
  };

  let toolu = dispatch_and_verify(&toolu_label, &workflow_sha, deadline, &mut guard)?;
  let reference = dispatch_and_verify(&reference_label, &workflow_sha, deadline, &mut guard)?;

  if toolu.artifact_bytes != ARTIFACT_BYTES {
    return Err(
      "toolu lane artifact container-73.txt did not contain the exact expected bytes".into(),
    );
  }
  if reference.artifact_bytes != ARTIFACT_BYTES {
    return Err(
      "reference lane artifact container-73.txt did not contain the exact expected bytes".into(),
    );
  }

  wait_for_docker_resource_absent("container", &toolu.container_id, deadline)?;
  wait_for_docker_resource_absent("network", &toolu.network, deadline)?;
  wait_for_docker_resource_absent("container", &reference.container_id, deadline)?;
  wait_for_docker_resource_absent("network", &reference.network, deadline)?;
  Ok(())
}

fn require_token_env() -> Result<(), Box<dyn Error>> {
  let present = ["GH_TOKEN", "GITHUB_TOKEN"]
    .iter()
    .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()));
  if present {
    Ok(())
  } else {
    Err("live test selected but neither GH_TOKEN nor GITHUB_TOKEN is set".into())
  }
}

fn required_label(name: &str) -> Result<String, Box<dyn Error>> {
  let label =
    std::env::var(name).map_err(|error| format!("invalid live test label {name}: {error}"))?;
  let valid = label.starts_with("toolu-73-")
    && !label.is_empty()
    && label
      .bytes()
      .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'));
  if valid {
    Ok(label)
  } else {
    Err(
      format!(
        "{name} must start with toolu-73- and contain only letters, digits, '-', '_', or '.'"
      )
      .into(),
    )
  }
}

fn require_cli(program: &str, args: &[&str]) -> Result<(), Box<dyn Error>> {
  let mut command = Command::new(program);
  command.args(args);
  if program == "gh" {
    command.env("GH_HOST", "github.com");
  }
  let output = capture(command, &format!("prerequisite `{program}`"))?;
  if output.status.success() {
    Ok(())
  } else {
    Err(
      format!(
        "live test selected but prerequisite `{program} {}` failed with {}",
        args.join(" "),
        output.status
      )
      .into(),
    )
  }
}

fn branch_sha() -> Result<String, Box<dyn Error>> {
  let output = gh_capture(&[
    "api".to_owned(),
    format!("repos/{REPOSITORY}/commits/{BRANCH}"),
  ])?;
  if !output.status.success() {
    return Err("GitHub could not resolve the fixed issue 73 branch SHA".into());
  }
  let sha = json_string(&output.stdout, "sha")?;
  if sha.len() == 40 && sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
    Ok(sha)
  } else {
    Err("GitHub returned an invalid SHA for the fixed issue 73 branch".into())
  }
}

fn dispatch_and_verify(
  label: &str,
  sha: &str,
  deadline: Instant,
  guard: &mut OwnedRunGuard,
) -> Result<LaneResult, Box<dyn Error>> {
  let prior = matching_run_ids(label, sha)?;
  gh_success(&[
    "workflow".to_owned(),
    "run".to_owned(),
    WORKFLOW.to_owned(),
    "--repo".to_owned(),
    REPOSITORY.to_owned(),
    "--ref".to_owned(),
    BRANCH.to_owned(),
    "--field".to_owned(),
    format!("runner_label={label}"),
  ])?;
  let run_id = wait_for_new_run(label, sha, &prior, deadline)?;
  guard.add(run_id);
  println!("job-container-73: label={label} run_id={run_id} workflow_sha={sha}");
  let conclusion = wait_for_terminal(run_id, deadline)?;
  guard.completed(run_id);
  if conclusion != "success" {
    return Err(format!("owned run {run_id} concluded `{conclusion}` instead of `success`").into());
  }
  assert_log_markers(run_id)?;
  download_lane_artifact(run_id)
}

fn matching_run_ids(label: &str, sha: &str) -> Result<BTreeSet<u64>, Box<dyn Error>> {
  let rows = gh_capture(&[
    "run".to_owned(),
    "list".to_owned(),
    "--repo".to_owned(),
    REPOSITORY.to_owned(),
    "--workflow".to_owned(),
    WORKFLOW.to_owned(),
    "--branch".to_owned(),
    BRANCH.to_owned(),
    "--limit".to_owned(),
    "100".to_owned(),
    "--json".to_owned(),
    "databaseId,displayTitle,headSha".to_owned(),
  ])?;
  if !rows.status.success() {
    return Err("GitHub could not list workflow runs for issue 73 discovery".into());
  }
  let wanted_title = format!("{RUN_PREFIX}{label}");
  let runs = json_array(&rows.stdout)?;
  let ids = runs
    .iter()
    .filter_map(|run| {
      let id = run.get("databaseId")?.as_u64()?;
      let title = run.get("displayTitle")?.as_str()?;
      let head_sha = run.get("headSha")?.as_str()?;
      (title == wanted_title && head_sha == sha).then_some(id)
    })
    .collect::<BTreeSet<_>>();
  Ok(ids)
}

fn wait_for_new_run(
  label: &str,
  sha: &str,
  prior: &BTreeSet<u64>,
  deadline: Instant,
) -> Result<u64, Box<dyn Error>> {
  loop {
    if Instant::now() >= deadline {
      return Err(
        format!("timed out within {TIMEOUT:?} waiting for a new {RUN_PREFIX}{label} run at {sha}")
          .into(),
      );
    }
    let new_ids = matching_run_ids(label, sha)?;
    let mut discovered = new_ids.difference(prior);
    let Some(run_id) = discovered.next().copied() else {
      thread::sleep(POLL_INTERVAL);
      continue;
    };
    if discovered.next().is_some() {
      return Err(
        format!("found more than one new owned run for label {label}; refusing to select one")
          .into(),
      );
    }
    return Ok(run_id);
  }
}

fn wait_for_terminal(run_id: u64, deadline: Instant) -> Result<String, Box<dyn Error>> {
  loop {
    if Instant::now() >= deadline {
      return Err(format!("timed out within {TIMEOUT:?} waiting for owned run {run_id}").into());
    }
    let state = run_state(run_id)?;
    if state.status == "completed" {
      return Ok(state.conclusion);
    }
    thread::sleep(POLL_INTERVAL);
  }
}

fn run_state(run_id: u64) -> Result<RunState, Box<dyn Error>> {
  let output = gh_capture(&[
    "run".to_owned(),
    "view".to_owned(),
    run_id.to_string(),
    "--repo".to_owned(),
    REPOSITORY.to_owned(),
    "--json".to_owned(),
    "status,conclusion".to_owned(),
  ])?;
  if !output.status.success() {
    return Err(format!("could not read owned run {run_id} state").into());
  }
  let status = json_string(&output.stdout, "status")?;
  let value = json_value(&output.stdout)?;
  let conclusion = value
    .get("conclusion")
    .and_then(serde_json::Value::as_str)
    .unwrap_or_default()
    .to_owned();
  Ok(RunState { conclusion, status })
}

fn assert_log_markers(run_id: u64) -> Result<(), Box<dyn Error>> {
  let metadata = gh_capture(&[
    "run".to_owned(),
    "view".to_owned(),
    run_id.to_string(),
    "--repo".to_owned(),
    REPOSITORY.to_owned(),
    "--json".to_owned(),
    "jobs".to_owned(),
  ])?;
  if !metadata.status.success() {
    return Err(format!("could not read owned run {run_id} jobs").into());
  }
  let value = json_value(&metadata.stdout)?;
  let jobs = value
    .get("jobs")
    .and_then(serde_json::Value::as_array)
    .ok_or("missing jobs")?;
  let [job] = jobs.as_slice() else {
    return Err("expected one container job".into());
  };
  let job_id = job
    .get("databaseId")
    .and_then(serde_json::Value::as_u64)
    .ok_or("missing job id")?;
  // The combined job log retains pre/main/post even when step logs reuse IDs.
  let output = gh_capture(&[
    "api".to_owned(),
    "--allow-escape-sequences".to_owned(),
    format!("repos/{REPOSITORY}/actions/jobs/{job_id}/logs"),
  ])?;
  if !output.status.success() {
    return Err(format!("could not download owned run {run_id} logs").into());
  }
  let log = std::str::from_utf8(&output.stdout)?;
  for marker in MARKERS {
    if !log.contains(marker) {
      return Err(format!("owned run {run_id} log is missing marker {marker}").into());
    }
  }
  Ok(())
}

fn download_lane_artifact(run_id: u64) -> Result<LaneResult, Box<dyn Error>> {
  let temp = tempfile::tempdir()?;
  gh_success(&[
    "run".to_owned(),
    "download".to_owned(),
    run_id.to_string(),
    "--repo".to_owned(),
    REPOSITORY.to_owned(),
    "--name".to_owned(),
    ARTIFACT_NAME.to_owned(),
    "--dir".to_owned(),
    temp.path().to_string_lossy().into_owned(),
  ])?;
  let artifact_bytes = fs::read(unique_named_file(temp.path(), "container-73.txt")?)?;
  let runtime = fs::read_to_string(unique_named_file(temp.path(), "container-73-runtime.txt")?)?;
  let (container_id, network) = parse_runtime_metadata(&runtime)?;
  Ok(LaneResult {
    artifact_bytes,
    container_id,
    network,
  })
}

fn parse_runtime_metadata(runtime: &str) -> Result<(String, String), Box<dyn Error>> {
  if !runtime.ends_with('\n') || runtime.contains('\r') {
    return Err("container-73-runtime.txt must be exactly two LF-terminated lines".into());
  }
  let lines = runtime.lines().collect::<Vec<_>>();
  let [container_id, network] = lines.as_slice() else {
    return Err(
      "container-73-runtime.txt must contain non-empty container id and network lines".into(),
    );
  };
  validate_docker_resource("container", container_id)?;
  validate_docker_resource("network", network)?;
  Ok(((*container_id).to_owned(), (*network).to_owned()))
}

fn wait_for_docker_resource_absent(
  kind: &str,
  name: &str,
  deadline: Instant,
) -> Result<(), Box<dyn Error>> {
  loop {
    match docker_resource_exists(kind, name)? {
      false => return Ok(()),
      true if Instant::now() < deadline => thread::sleep(POLL_INTERVAL),
      true => {
        return Err(format!("owned {kind} {name} still exists after the {TIMEOUT:?} bound").into());
      },
    }
  }
}

fn docker_resource_exists(kind: &str, name: &str) -> Result<bool, Box<dyn Error>> {
  validate_docker_resource(kind, name)?;
  let mut command = Command::new("docker");
  command.args(["inspect", "--type", kind, "--", name]);
  let output = capture(command, &format!("docker inspect for owned {kind}"))?;
  if output.status.success() {
    return Ok(true);
  }
  let error_text = String::from_utf8_lossy(&output.stderr);
  let container_missing = format!("No such container: {name}");
  let network_missing = format!("network {name} not found");
  if error_text.contains(&container_missing) || error_text.contains(&network_missing) {
    Ok(false)
  } else {
    Err(format!("docker inspect for owned {kind} {name} failed unexpectedly with {}; not treating it as cleanup", output.status).into())
  }
}

fn validate_docker_resource(kind: &str, name: &str) -> Result<(), Box<dyn Error>> {
  let valid = match kind {
    "container" => name.len() == 64 && name.bytes().all(|byte| byte.is_ascii_hexdigit()),
    "network" => {
      !name.is_empty()
        && name
          .bytes()
          .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    },
    _ => false,
  };
  if valid {
    Ok(())
  } else {
    Err(format!("container-73 runtime metadata contains an invalid {kind} identifier").into())
  }
}

fn unique_named_file(root: &Path, name: &str) -> Result<PathBuf, Box<dyn Error>> {
  let mut matches = Vec::new();
  collect_named_files(root, name, &mut matches)?;
  match matches.as_slice() {
    [path] => Ok(path.clone()),
    [] => Err(format!("artifact download did not contain {name}").into()),
    _ => Err(format!("artifact download contained more than one {name}").into()),
  }
}

fn collect_named_files(root: &Path, name: &str, matches: &mut Vec<PathBuf>) -> std::io::Result<()> {
  for entry in fs::read_dir(root)? {
    let path = entry?.path();
    if path.is_dir() {
      collect_named_files(&path, name, matches)?;
    } else if path.file_name().is_some_and(|file_name| file_name == name) {
      matches.push(path);
    }
  }
  Ok(())
}

fn gh_capture(args: &[String]) -> Result<Captured, Box<dyn Error>> {
  let mut command = Command::new("gh");
  command.args(args).env("GH_HOST", "github.com");
  capture(command, "gh command")
}

fn gh_success(args: &[String]) -> Result<(), Box<dyn Error>> {
  let output = gh_capture(args)?;
  output.status.success().then_some(()).ok_or_else(|| {
    "gh command failed; output intentionally withheld to avoid leaking credentials or raw logs"
      .into()
  })
}

fn json_value(bytes: &[u8]) -> Result<serde_json::Value, Box<dyn Error>> {
  Ok(serde_json::from_slice(bytes)?)
}

fn json_array(bytes: &[u8]) -> Result<Vec<serde_json::Value>, Box<dyn Error>> {
  json_value(bytes)?
    .as_array()
    .cloned()
    .ok_or_else(|| "gh returned a non-array workflow-run listing".into())
}

fn json_string(bytes: &[u8], field: &str) -> Result<String, Box<dyn Error>> {
  json_value(bytes)?
    .get(field)
    .and_then(serde_json::Value::as_str)
    .map(str::to_owned)
    .ok_or_else(|| format!("gh JSON was missing string field {field}").into())
}
