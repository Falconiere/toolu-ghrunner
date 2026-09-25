//! Explicit GitHub.com and GHES Checks API comparison for issue 82.
//!
//! The branch push starts one workflow with two dedicated self-hosted jobs.
//! These ignored tests fail if a runner, token, run, version, or annotation is
//! absent. A green API comparison still requires separate Checks UI inspection.

#![cfg(feature = "live")]

use std::collections::BTreeMap;
use std::error::Error;
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::Value;

type TestResult<T> = Result<T, Box<dyn Error>>;

const WORKFLOW: &str = "annotation-82-live.yml";
const BRANCH: &str = "feat/82-send-annotations-in-the-run";
const SOURCE: &str = ".github/annotation-82-source.txt";
const POLL: Duration = Duration::from_secs(5);
const RUN_TIMEOUT: Duration = Duration::from_secs(15 * 60);

struct Host {
  hostname: String,
  repo: String,
  branch: String,
  token: String,
  toolu_name: String,
  reference_name: String,
  toolu_version: String,
  reference_version: String,
}

impl Host {
  fn github() -> TestResult<Self> {
    Ok(Self {
      hostname: "github.com".to_owned(),
      repo: "Falconiere/toolu-ghrunner".to_owned(),
      branch: BRANCH.to_owned(),
      token: required("GH_TOKEN").or_else(|_| required("GITHUB_TOKEN"))?,
      toolu_name: required("TOOLU_ANNOTATION_TOOLU_NAME")?,
      reference_name: required("TOOLU_ANNOTATION_REFERENCE_NAME")?,
      toolu_version: required("TOOLU_ANNOTATION_TOOLU_VERSION")?,
      reference_version: required("TOOLU_ANNOTATION_REFERENCE_VERSION")?,
    })
  }

  fn ghes() -> TestResult<Self> {
    let raw_url = required("TOOLU_ANNOTATION_GHES_URL")?;
    let url = url::Url::parse(&raw_url)?;
    let hostname = url.host_str().ok_or("GHES URL has no host")?;
    let hostname = url
      .port()
      .map_or_else(|| hostname.to_owned(), |port| format!("{hostname}:{port}"));
    let branch = required("TOOLU_ANNOTATION_GHES_BRANCH")?;
    if branch != BRANCH {
      return Err(format!("GHES branch must be {BRANCH} for the workflow push trigger").into());
    }
    Ok(Self {
      hostname,
      repo: required("TOOLU_ANNOTATION_GHES_REPO")?,
      branch,
      token: required("TOOLU_ANNOTATION_GHES_TOKEN")?,
      toolu_name: required("TOOLU_ANNOTATION_GHES_TOOLU_NAME")?,
      reference_name: required("TOOLU_ANNOTATION_GHES_REFERENCE_NAME")?,
      toolu_version: required("TOOLU_ANNOTATION_GHES_TOOLU_VERSION")?,
      reference_version: required("TOOLU_ANNOTATION_GHES_REFERENCE_VERSION")?,
    })
  }

  fn gh(&self, endpoint: &str, binary: bool) -> TestResult<Vec<u8>> {
    let mut command = Command::new("gh");
    command
      .arg("api")
      .arg("--hostname")
      .arg(&self.hostname)
      .arg(endpoint)
      .env("GH_HOST", &self.hostname)
      .env("GH_TOKEN", &self.token)
      .env("GITHUB_TOKEN", &self.token)
      .env("GH_ENTERPRISE_TOKEN", &self.token);
    if binary {
      command.arg("--allow-escape-sequences");
    }
    let output = command.output()?;
    if !output.status.success() {
      return Err(format!("gh api failed for {endpoint}: {}", output.status).into());
    }
    Ok(output.stdout)
  }

  fn api(&self, endpoint: &str) -> TestResult<Value> {
    Ok(serde_json::from_slice(&self.gh(endpoint, false)?)?)
  }

  fn branch_sha(&self) -> TestResult<String> {
    let row = self.api(&format!("repos/{}/commits/{}", self.repo, self.branch))?;
    let sha = field_str(&row, "sha")?.to_owned();
    if sha.len() != 40 || !sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
      return Err("branch SHA is not a 40-character hexadecimal revision".into());
    }
    Ok(sha)
  }

  fn verify_runners(&self) -> TestResult<()> {
    let listing = self.api(&format!("repos/{}/actions/runners?per_page=100", self.repo))?;
    let runners = field_array(&listing, "runners")?;
    let mut platform = None;
    for (name, label) in [
      (&self.toolu_name, "toolu-82-tool"),
      (&self.reference_name, "toolu-82-reference"),
    ] {
      let matches = runners
        .iter()
        .filter(|runner| runner.get("name").and_then(Value::as_str) == Some(name.as_str()))
        .collect::<Vec<_>>();
      let [runner] = matches.as_slice() else {
        return Err(
          format!(
            "expected exactly one registered runner named {name}, got {}",
            matches.len()
          )
          .into(),
        );
      };
      let has_label = field_array(runner, "labels")?
        .iter()
        .any(|entry| entry.get("name").and_then(Value::as_str) == Some(label));
      if field_str(runner, "status")? != "online" || !has_label {
        return Err(format!("runner {name} must be online with label {label}").into());
      }
      let current = runner_platform(runner)?;
      if platform.as_ref().is_some_and(|prior| prior != &current) {
        return Err(
          "toolu and reference runners must have matching OS and architecture labels".into(),
        );
      }
      platform = Some(current);
    }
    Ok(())
  }

  async fn run_at_sha(&self, sha: &str, deadline: Instant) -> TestResult<Value> {
    loop {
      let listing = self.api(&format!(
        "repos/{}/actions/workflows/{WORKFLOW}/runs?branch={}&per_page=100",
        self.repo, self.branch
      ))?;
      let rows = field_array(&listing, "workflow_runs")?;
      if let Some(run) = rows.iter().find(|run| {
        run.get("head_sha").and_then(Value::as_str) == Some(sha)
          && run.get("event").and_then(Value::as_str) == Some("push")
      }) {
        return Ok(run.clone());
      }
      if Instant::now() >= deadline {
        return Err(format!("no pushed {WORKFLOW} run appeared at {sha}").into());
      }
      tokio::time::sleep(POLL).await;
    }
  }

  async fn completed_run(&self, run_id: u64, sha: &str, deadline: Instant) -> TestResult<Value> {
    loop {
      let run = self.api(&format!("repos/{}/actions/runs/{run_id}", self.repo))?;
      if field_str(&run, "head_sha")? != sha {
        return Err(format!("run {run_id} moved to a different SHA").into());
      }
      if field_str(&run, "status")? == "completed" {
        let conclusion = field_str(&run, "conclusion")?;
        if conclusion != "success" {
          return Err(format!("run {run_id} did not succeed: {conclusion}").into());
        }
        return Ok(run);
      }
      if Instant::now() >= deadline {
        return Err(format!("run {run_id} did not complete within {RUN_TIMEOUT:?}").into());
      }
      tokio::time::sleep(POLL).await;
    }
  }
}

fn runner_platform(runner: &Value) -> TestResult<(String, String)> {
  let labels = field_array(runner, "labels")?;
  let find = |names: &[&str]| {
    labels
      .iter()
      .filter_map(|entry| entry.get("name").and_then(Value::as_str))
      .find(|label| names.contains(label))
      .map(str::to_owned)
  };
  let os = find(&["Linux", "macOS", "Windows"]).ok_or("runner has no standard OS label")?;
  let arch = find(&["X64", "ARM64", "ARM"]).ok_or("runner has no standard architecture label")?;
  Ok((os, arch))
}

fn required(name: &str) -> TestResult<String> {
  let value =
    std::env::var(name).map_err(|error| format!("missing required live input {name}: {error}"))?;
  if value.trim().is_empty() {
    return Err(format!("required live input {name} is empty").into());
  }
  Ok(value)
}

fn field_str<'a>(value: &'a Value, key: &str) -> TestResult<&'a str> {
  value
    .get(key)
    .and_then(Value::as_str)
    .ok_or_else(|| format!("missing string {key}: {value}").into())
}

fn field_u64(value: &Value, key: &str) -> TestResult<u64> {
  value
    .get(key)
    .and_then(Value::as_u64)
    .ok_or_else(|| format!("missing integer {key}: {value}").into())
}

fn field_array<'a>(value: &'a Value, key: &str) -> TestResult<&'a [Value]> {
  value
    .get(key)
    .and_then(Value::as_array)
    .map(Vec::as_slice)
    .ok_or_else(|| format!("missing array {key}: {value}").into())
}

fn local_sha() -> TestResult<String> {
  let output = Command::new("git").args(["rev-parse", "HEAD"]).output()?;
  if !output.status.success() {
    return Err("git rev-parse HEAD failed".into());
  }
  Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

async fn verify(host: &Host) -> TestResult<()> {
  if host.toolu_name == host.reference_name {
    return Err("toolu and reference runner names must differ".into());
  }
  let sha = host.branch_sha()?;
  if sha != local_sha()? {
    return Err(format!("live branch SHA {sha} does not match this checkout").into());
  }
  host.verify_runners()?;
  let deadline = Instant::now() + RUN_TIMEOUT;
  let run = host.run_at_sha(&sha, deadline).await?;
  let run_id = field_u64(&run, "id")?;
  host.completed_run(run_id, &sha, deadline).await?;
  let listing = host.api(&format!(
    "repos/{}/actions/runs/{run_id}/jobs?per_page=100",
    host.repo
  ))?;
  let jobs = field_array(&listing, "jobs")?;
  let toolu = lane(
    host,
    jobs,
    "toolu",
    &host.toolu_name,
    &host.toolu_version,
    &sha,
  )?;
  let reference = lane(
    host,
    jobs,
    "reference",
    &host.reference_name,
    &host.reference_version,
    &sha,
  )?;
  if toolu != reference {
    return Err("toolu and pinned official runner Checks annotations differ".into());
  }
  println!(
    "annotation-82: host={} sha={sha} run={run_id} checks-ui=https://{}/actions/runs/{run_id}",
    host.hostname, host.hostname
  );
  Ok(())
}

fn lane(
  host: &Host,
  jobs: &[Value],
  name: &str,
  runner: &str,
  version: &str,
  sha: &str,
) -> TestResult<BTreeMap<String, Value>> {
  let job_name = format!("annotation-82-{name}");
  let job = jobs
    .iter()
    .find(|job| job.get("name").and_then(Value::as_str) == Some(&job_name))
    .ok_or_else(|| format!("missing {job_name} job"))?;
  if field_str(job, "conclusion")? != "success" || field_str(job, "runner_name")? != runner {
    return Err(format!("{job_name} did not succeed on required runner {runner}: {job}").into());
  }
  let steps = field_array(job, "steps")?;
  for step_name in ["Shell annotations", "Composite annotations"] {
    if !steps
      .iter()
      .any(|step| step.get("name").and_then(Value::as_str) == Some(step_name))
    {
      return Err(format!("{job_name} is missing {step_name} step").into());
    }
  }
  let job_id = field_u64(job, "id")?;
  let log = String::from_utf8(host.gh(
    &format!("repos/{}/actions/jobs/{job_id}/logs", host.repo),
    true,
  )?)?;
  if !log.contains(version) {
    return Err(format!("{job_name} log does not prove required binary version {version}").into());
  }
  let check_url = field_str(job, "check_run_url")?;
  let check_id = check_url
    .rsplit('/')
    .next()
    .ok_or("check_run_url has no id")?;
  let check = host.api(&format!("repos/{}/check-runs/{check_id}", host.repo))?;
  if field_str(&check, "head_sha")? != sha {
    return Err(format!("{job_name} check run has the wrong SHA").into());
  }
  let annotations = host.api(&format!(
    "repos/{}/check-runs/{check_id}/annotations?per_page=100",
    host.repo
  ))?;
  let rows = annotations
    .as_array()
    .ok_or("Checks annotations response is not an array")?;
  let found = validate_annotations(rows)?;
  println!(
    "annotation-82: {name} job={job_id} runner={runner} version={version} check={check_url}"
  );
  Ok(found)
}

fn validate_annotations(rows: &[Value]) -> TestResult<BTreeMap<String, Value>> {
  let expected = [
    (
      "Compile: detail",
      "failure",
      "boom\n***",
      4,
      4,
      Some(2),
      Some(8),
    ),
    ("Warn: detail", "warning", "warning text", 7, 7, None, None),
    ("Notice: detail", "notice", "notice text", 9, 9, None, None),
    (
      "Nested: first",
      "warning",
      "composite one",
      11,
      11,
      None,
      None,
    ),
    (
      "Nested: second",
      "notice",
      "composite two",
      12,
      12,
      None,
      None,
    ),
  ];
  let mut found = BTreeMap::new();
  for (title, level, message, start, end, start_col, end_col) in expected {
    let matches = rows
      .iter()
      .filter(|row| row.get("title").and_then(Value::as_str) == Some(title))
      .collect::<Vec<_>>();
    let [row] = matches.as_slice() else {
      return Err(
        format!(
          "expected one Checks annotation titled {title}, got {}",
          matches.len()
        )
        .into(),
      );
    };
    if field_str(row, "annotation_level")? != level
      || field_str(row, "message")? != message
      || field_str(row, "path")? != SOURCE
      || field_u64(row, "start_line")? != start
      || field_u64(row, "end_line")? != end
      || row.get("start_column").and_then(Value::as_u64) != start_col
      || row.get("end_column").and_then(Value::as_u64) != end_col
    {
      return Err(
        format!("Checks annotation {title} has wrong severity, text, or location: {row}").into(),
      );
    }
    found.insert(title.to_owned(), (*row).clone());
  }
  Ok(found)
}

#[tokio::test]
#[ignore = "requires pushed branch, online paired runners and explicit version pins"]
async fn annotation_matches_reference() -> TestResult<()> {
  verify(&Host::github()?).await
}

#[tokio::test]
#[ignore = "requires a supported GHES repo with this pushed branch and paired runners"]
async fn annotation_matches_ghes_reference() -> TestResult<()> {
  verify(&Host::ghes()?).await
}
