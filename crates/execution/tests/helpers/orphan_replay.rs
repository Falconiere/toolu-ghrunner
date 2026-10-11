//! Shared replay harness for `orphan_cleanup_test.rs` (issue #89).
//!
//! Included via `#[path = "helpers/orphan_replay.rs"] mod orphan_replay;` —
//! Cargo does not compile `tests/helpers/**` as a test target of its own.
//! Replays the sanitized #68 GitHub.com acquisition through the production
//! `Runner` path and observes real processes with `ps`.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use execution::Runner;
use execution::node::runtime::{node_binary_path, node_cache_dir, node_version_for};
use shared::{
  AgentJobRequestMessage, Conclusion, DictEntry, RunnerConfig, RunnerEvent, SecretMasker,
  TemplateToken, VariableValue,
};
use tokio_util::sync::CancellationToken;

pub type TestResult<T = ()> = Result<T, Box<dyn Error>>;

const CAPTURE: &str = include_str!("../incoming_contexts_matrix_0.json");
pub const PROBE: &str = "printf 'TRACK89|run|%s\\n' \"${RUNNER_TRACKING_ID-unset}\"";
pub const HEADER: &str = "Cleaning up orphan processes";
/// Step body + 2 s pipe drain + 15 s cleanup, with slack for a loaded host.
const JOB_BOUND: Duration = Duration::from_secs(60);
/// Replay deadline, derived from `JOB_BOUND` with headroom so a slow job
/// fails the elapsed assertion rather than this timeout.
const RUN_TIMEOUT: Duration = Duration::from_secs(JOB_BOUND.as_secs() * 4);

// ---------------------------------------------------------------------------
// Captured-message shaping
// ---------------------------------------------------------------------------

pub fn literal(value: &str) -> TemplateToken {
  TemplateToken {
    token_type: 0,
    lit: Some(value.to_owned()),
    ..TemplateToken::default()
  }
}

pub fn mapping(values: &[(&str, &str)]) -> TemplateToken {
  TemplateToken {
    token_type: 2,
    d: Some(
      values
        .iter()
        .map(|(key, value)| DictEntry {
          key: literal(key),
          value: literal(value),
        })
        .collect(),
    ),
    ..TemplateToken::default()
  }
}

pub fn variable(job: &mut AgentJobRequestMessage, key: &str, value: &str) {
  job.variables.insert(
    key.to_owned(),
    VariableValue {
      value: value.to_owned(),
      is_secret: false,
    },
  );
}

pub fn set_script(step: &mut shared::ActionStep, body: &str) -> TestResult {
  let script = step
    .inputs
    .d
    .as_mut()
    .and_then(|entries| {
      entries
        .iter_mut()
        .find(|entry| entry.key.to_string_value() == Some("script"))
    })
    .ok_or("captured script token absent")?;
  script.value = literal(body);
  Ok(())
}

/// The captured job reduced to its first `run:` step carrying `body`.
pub fn script_job(body: &str) -> TestResult<AgentJobRequestMessage> {
  let mut job: AgentJobRequestMessage = serde_json::from_str(CAPTURE)?;
  job
    .steps
    .retain(|step| step.context_name.as_deref() == Some("__run"));
  set_script(
    job.steps.first_mut().ok_or("captured script step absent")?,
    body,
  )?;
  Ok(job)
}

/// The captured job reduced to its `run:` step plus the local Node action step.
pub fn script_and_node_job(
  body: &str,
  node_env: &[(&str, &str)],
) -> TestResult<AgentJobRequestMessage> {
  let mut job: AgentJobRequestMessage = serde_json::from_str(CAPTURE)?;
  job
    .steps
    .retain(|step| matches!(step.context_name.as_deref(), Some("__run" | "__self")));
  let mut steps = job.steps.iter_mut();
  set_script(steps.next().ok_or("captured script step absent")?, body)?;
  let node = steps.next().ok_or("captured action step absent")?;
  node.reference.path = Some("./.github/actions/orphan-89".to_owned());
  node.environment = (!node_env.is_empty()).then(|| mapping(node_env));
  Ok(job)
}

// ---------------------------------------------------------------------------
// Replay harness
// ---------------------------------------------------------------------------

pub struct Replay {
  _root: tempfile::TempDir,
  config: RunnerConfig,
  pub pids: PathBuf,
}

impl Replay {
  pub async fn new() -> TestResult<Self> {
    let root = tempfile::tempdir()?;
    let config = RunnerConfig {
      data_dir: root.path().join("data"),
      workspace_root: root.path().join("work"),
      workspace_gc_hours: 0,
      ..RunnerConfig::default()
    };
    let pids = root.path().join("pids");
    std::fs::create_dir_all(&pids)?;
    seed_node(&config.data_dir).await?;
    Ok(Self {
      _root: root,
      config,
      pids,
    })
  }

  pub fn pids_dir(&self) -> String {
    self.pids.to_string_lossy().into_owned()
  }

  pub fn install_node_action(&self, job: &AgentJobRequestMessage) -> TestResult {
    let root = self
      .config
      .workspace_root
      .join(&job.job_id)
      .join(".github/actions/orphan-89");
    std::fs::create_dir_all(&root)?;
    std::fs::write(
      root.join("action.yml"),
      include_str!("../orphan_89_action.yml"),
    )?;
    std::fs::write(root.join("main.js"), include_str!("../orphan_89_main.js"))?;
    std::fs::write(root.join("post.js"), include_str!("../orphan_89_post.js"))?;
    Ok(())
  }

  pub fn pid(&self, name: &str) -> TestResult<u32> {
    Ok(
      std::fs::read_to_string(self.pids.join(name))?
        .trim()
        .parse()?,
    )
  }

  /// Run `job` to the end of its event stream. `on_event` may fire the job's
  /// cancellation / shutdown tokens while it runs.
  pub async fn run(
    &self,
    job: AgentJobRequestMessage,
    mut on_event: impl FnMut(&RunnerEvent, &CancellationToken, &CancellationToken),
  ) -> TestResult<Finished> {
    let runner = Runner::new(
      self.config.clone(),
      Arc::new(Mutex::new(SecretMasker::new())),
    );
    let cancel = CancellationToken::new();
    let shutdown = CancellationToken::new();
    let started = Instant::now();
    let mut receiver = runner.execute_job_with_shutdown(job, cancel.clone(), shutdown.clone());
    let mut events = Vec::new();
    tokio::time::timeout(RUN_TIMEOUT, async {
      while let Some(event) = receiver.recv().await {
        on_event(&event, &cancel, &shutdown);
        events.push(event);
      }
    })
    .await
    .map_err(|error| {
      format!(
        "replay timed out: {error}; last events: {:?}",
        events.iter().rev().take(8).collect::<Vec<_>>()
      )
    })?;
    Ok(Finished {
      events,
      elapsed: started.elapsed(),
    })
  }
}

pub struct Finished {
  events: Vec<RunnerEvent>,
  elapsed: Duration,
}

impl Finished {
  pub fn conclusion(&self) -> Option<Conclusion> {
    self.events.iter().find_map(|event| {
      if let RunnerEvent::JobCompleted { conclusion, .. } = event {
        Some(*conclusion)
      } else {
        None
      }
    })
  }

  pub fn lines(&self) -> Vec<&str> {
    self
      .events
      .iter()
      .filter_map(|event| {
        if let RunnerEvent::Log { line, .. } = event {
          Some(line.as_str())
        } else {
          None
        }
      })
      .collect()
  }

  /// `TRACK89|<stage>|<value>` markers in arrival order as `(stage, value)`.
  pub fn markers(&self) -> Vec<(String, String)> {
    self
      .lines()
      .into_iter()
      .filter_map(|line| {
        let rest = line.strip_prefix("TRACK89|")?;
        let (stage, value) = rest.split_once('|')?;
        Some((stage.to_owned(), value.to_owned()))
      })
      .collect()
  }

  pub fn has_line(&self, needle: &str) -> bool {
    self.lines().iter().any(|line| line.contains(needle))
  }

  pub fn terminated(&self, pid: u32) -> bool {
    self.has_line(&format!("Terminate orphan process: pid ({pid})"))
  }
}

pub async fn seed_node(data: &Path) -> TestResult {
  let output = tokio::process::Command::new("node")
    .args(["-e", "process.stdout.write(process.execPath)"])
    .output()
    .await?;
  assert!(output.status.success(), "Node is required for this test");
  let path = String::from_utf8(output.stdout)?;
  let binary = node_binary_path(&node_cache_dir(data, node_version_for(20)));
  std::fs::create_dir_all(binary.parent().ok_or("node cache parent missing")?)?;
  std::os::unix::fs::symlink(path.trim(), &binary)?;
  Ok(())
}

// ---------------------------------------------------------------------------
// Real process observation
// ---------------------------------------------------------------------------

/// `ps` state for `pid`, or `None` when no such process exists.
pub fn ps_state(pid: u32) -> TestResult<Option<String>> {
  let output = std::process::Command::new("ps")
    .args(["-o", "stat=", "-p", &pid.to_string()])
    .output()?;
  let state = String::from_utf8(output.stdout)?.trim().to_owned();
  Ok((!state.is_empty()).then_some(state))
}

pub fn is_dead(pid: u32) -> TestResult<bool> {
  Ok(ps_state(pid)?.is_none_or(|state| state.starts_with('Z')))
}

/// Poll briefly: a killed orphan is reaped by init asynchronously.
pub async fn assert_dead(pid: u32, label: &str) -> TestResult {
  for _ in 0..50 {
    if is_dead(pid)? {
      return Ok(());
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
  }
  Err(format!("{label} pid {pid} still alive: {:?}", ps_state(pid)?).into())
}

pub fn assert_alive(pid: u32, label: &str) -> TestResult {
  let state = ps_state(pid)?;
  assert!(
    state.as_deref().is_some_and(|s| !s.starts_with('Z')),
    "{label} pid {pid} should be alive, ps state: {state:?}"
  );
  Ok(())
}

/// Best-effort removal of a process the test deliberately left alive.
pub fn kill(pid: u32) {
  let _ = std::process::Command::new("kill")
    .args(["-9", &pid.to_string()])
    .status();
}

pub async fn wait_for_file(path: &Path) -> TestResult {
  for _ in 0..600 {
    if std::fs::metadata(path).is_ok_and(|meta| meta.len() > 0) {
      return Ok(());
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
  }
  Err(format!("{} never appeared", path.display()).into())
}

pub fn is_tracking_id(value: &str) -> bool {
  value.strip_prefix("github_").is_some_and(|uuid| {
    uuid.len() == 36
      && uuid
        .chars()
        .all(|c| c == '-' || c.is_ascii_digit() || ('a'..='f').contains(&c))
  })
}

// ---------------------------------------------------------------------------
// Step scripts
// ---------------------------------------------------------------------------

/// Bash that leaves four kinds of orphan behind, records their pids, then
/// runs `tail` (`exit 0`, `exit 1`, or a blocking wait).
pub fn orphan_script(pids: &str, tail: &str) -> String {
  format!(
    r#"P='{pids}'
{PROBE}
sleep 300 & echo $! > "$P/child"
bash -c 'sleep 301 >/dev/null 2>&1 & echo $! > "$1/grandchild"' _ "$P"
nohup sleep 302 >/dev/null 2>&1 & echo $! > "$P/nohup"
python3 -c 'import os,sys,time; os.setsid(); open(sys.argv[1],"w").write(str(os.getpid())); time.sleep(303)' "$P/detached" >/dev/null 2>&1 < /dev/null &
while [ ! -s "$P/detached" ] || [ ! -s "$P/grandchild" ]; do sleep 0.05; done
{tail}
"#
  )
}

/// Bash that detaches one `setsid` sleeper recording its pid as `name`.
pub fn detached_script(pids: &str, name: &str, tail: &str) -> String {
  format!(
    r#"P='{pids}'
{PROBE}
python3 -c 'import os,sys,time; os.setsid(); open(sys.argv[1],"w").write(str(os.getpid())); time.sleep(306)' "$P/{name}" >/dev/null 2>&1 < /dev/null &
while [ ! -s "$P/{name}" ]; do sleep 0.05; done
{tail}
"#
  )
}

pub const ORPHANS: [&str; 4] = ["child", "grandchild", "nohup", "detached"];

/// Every orphan kind is dead after the job, and each one named in `swept` got
/// a `Terminate orphan process` line. (On cancellation the step-group `killpg`
/// already reaps the non-detached kinds, so only `detached` reaches the sweep.)
pub async fn assert_orphans_cleaned(
  replay: &Replay,
  finished: &Finished,
  swept: &[&str],
) -> TestResult {
  assert!(
    finished.has_line(HEADER),
    "cleanup header missing: {:?}",
    finished.lines()
  );
  for name in ORPHANS {
    let pid = replay.pid(name)?;
    assert_dead(pid, name).await?;
    assert!(
      !swept.contains(&name) || finished.terminated(pid),
      "no Terminate line for {name} pid {pid}: {:?}",
      finished.lines()
    );
  }
  assert!(
    finished.elapsed < JOB_BOUND,
    "job took {:?}",
    finished.elapsed
  );
  Ok(())
}
