//! Job-end orphan process cleanup keyed on `RUNNER_TRACKING_ID` (issue #89).
//!
//! Mirrors actions/runner `JobExtension` (pinned `cab9d1c`, lines 586-600 and
//! 880-925): every host step process inherits a fresh per-job
//! `RUNNER_TRACKING_ID=github_<uuid>`, and when the job finishes every other
//! process whose environment still carries that id receives `SIGKILL`.
//! Linux reads `/proc/<pid>/environ`; macOS reads `sysctl(KERN_PROCARGS2)`.
//! Both expose the environment captured at `exec` and only for processes the
//! runner's user may inspect, so `sudo`, `env -i` and other users' processes
//! fall outside the sweep, exactly as upstream.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::time::{Duration, Instant};

use shared::{LogStream, RunnerEvent, VariableValue};
use sysinfo::{
  Pid, ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, Signal, System, UpdateKind,
};
use tokio::sync::mpsc;

/// Environment variable carrying the per-job tracking id.
pub(crate) const TRACKING_ENV: &str = "RUNNER_TRACKING_ID";
/// Job message variable that disables tracking when it parses as `false`.
const PROCESS_CLEAN_VARIABLE: &str = "process.clean";
/// Shared deadline for every scan, kill and exit poll of one sweep.
pub(crate) const ORPHAN_CLEANUP_TIMEOUT: Duration = Duration::from_secs(15);
/// Liveness backstop for a blocking scan stuck inside one `/proc` read.
const JOIN_BACKSTOP: Duration = Duration::from_secs(2);
/// Interval between exit polls of killed processes.
const EXIT_POLL: Duration = Duration::from_millis(100);
/// Job log header, upstream's wording.
const HEADER: &str = "Cleaning up orphan processes";

/// The tracking id of one job; `None` on the context when `process.clean=false`.
#[derive(Debug, Clone)]
pub struct ProcessTracking {
  id: String,
}

impl ProcessTracking {
  /// A fresh `github_<uuid v4>` id unless the job's `process.clean` is `false`.
  pub(crate) fn for_job(variables: &HashMap<String, VariableValue>) -> Option<Self> {
    process_clean_enabled(variables).then(|| Self {
      id: format!("github_{}", uuid::Uuid::new_v4()),
    })
  }

  /// The id written into host step process environments.
  pub fn id(&self) -> &str {
    &self.id
  }
}

/// Upstream `GetBoolean("process.clean") ?? true`: only a (trimmed,
/// case-insensitive) `false` disables tracking; anything else enables it.
pub(crate) fn process_clean_enabled(variables: &HashMap<String, VariableValue>) -> bool {
  !variables.iter().any(|(key, value)| {
    key.eq_ignore_ascii_case(PROCESS_CLEAN_VARIABLE)
      && value.value.trim().eq_ignore_ascii_case("false")
  })
}

/// Whether an environment block holds `RUNNER_TRACKING_ID=<id>` (the value
/// compared ASCII case-insensitively, as upstream's `OrdinalIgnoreCase`).
pub(crate) fn carries_id(environ: &[OsString], id: &str) -> bool {
  environ.iter().any(|entry| {
    let bytes = entry.as_encoded_bytes();
    bytes
      .iter()
      .position(|byte| *byte == b'=')
      .and_then(|split| bytes.split_at_checked(split))
      .is_some_and(|(key, value)| {
        key == TRACKING_ENV.as_bytes()
          && value
            .get(1..)
            .is_some_and(|value| value.eq_ignore_ascii_case(id.as_bytes()))
      })
  })
}

/// What one sweep did; logged as WARN lines and asserted by tests.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CleanupReport {
  /// Pids that received `SIGKILL` (or vanished) and are gone or zombies.
  pub terminated: Vec<u32>,
  /// Pids that could not be signalled or had not exited by the deadline.
  pub survivors: Vec<u32>,
  /// The shared deadline expired before the sweep finished.
  pub timed_out: bool,
  /// The runner could not see its own process, so the scan saw nothing useful.
  pub degraded: bool,
}

/// One process found carrying the id during a scan.
#[derive(Debug, Clone)]
pub(crate) struct Candidate {
  pub(crate) pid: u32,
  start_time: u64,
  name: String,
}

/// Result of one full process scan.
pub(crate) struct Scan {
  pub(crate) candidates: Vec<Candidate>,
  pub(crate) own_pid_seen: bool,
}

/// What `kill_verified` did with one candidate.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum KillOutcome {
  /// Re-verified and signalled.
  Killed,
  /// Gone, re-exec'd without the id, or a different process now: untouched.
  Skipped,
  /// Re-verified but the signal could not be delivered.
  Failed,
}

/// Run one job-end sweep, logging under the job (`step_id: ""`). Never fails:
/// every problem becomes a WARN line and the job conclusion is untouched.
pub(crate) async fn sweep(
  tracking: &ProcessTracking,
  events: &mpsc::Sender<RunnerEvent>,
) -> CleanupReport {
  job_log(events, HEADER.to_owned()).await;
  let id = tracking.id.clone();
  let tx = events.clone();
  let own_pid = std::process::id();
  let deadline = Instant::now() + ORPHAN_CLEANUP_TIMEOUT;
  let task = tokio::task::spawn_blocking(move || {
    sweep_blocking(&id, own_pid, deadline, |pid, name| {
      let line = format!("Terminate orphan process: pid ({pid}) ({name})");
      if tx.blocking_send(job_event(line)).is_err() {
        tracing::warn!(
          pid,
          "event channel closed; orphan termination line was dropped"
        );
      }
    })
  });
  let report = match tokio::time::timeout(ORPHAN_CLEANUP_TIMEOUT + JOIN_BACKSTOP, task).await {
    Ok(Ok(report)) => report,
    Ok(Err(error)) => {
      tracing::warn!(%error, "orphan cleanup task failed");
      CleanupReport::default()
    },
    Err(_elapsed) => CleanupReport {
      timed_out: true,
      ..CleanupReport::default()
    },
  };
  warn_on_report(events, &report).await;
  report
}

/// Scan, re-verify and kill until a pass finds no new candidate, then wait for
/// the killed processes to exit — all bounded by `deadline`.
pub(crate) fn sweep_blocking(
  id: &str,
  own_pid: u32,
  deadline: Instant,
  mut on_kill: impl FnMut(u32, &str),
) -> CleanupReport {
  let mut report = CleanupReport::default();
  let mut system = System::new();
  let mut seen = HashSet::new();
  let mut killed = Vec::new();
  'passes: loop {
    if Instant::now() >= deadline {
      report.timed_out = true;
      break;
    }
    let scan = scan_candidates(&mut system, id, own_pid);
    report.degraded |= !scan.own_pid_seen;
    let fresh: Vec<Candidate> = scan
      .candidates
      .into_iter()
      .filter(|candidate| seen.insert(candidate.pid))
      .collect();
    if fresh.is_empty() {
      break;
    }
    for candidate in fresh {
      if Instant::now() >= deadline {
        report.timed_out = true;
        break 'passes;
      }
      match kill_verified(&mut system, id, &candidate) {
        KillOutcome::Killed => {
          on_kill(candidate.pid, &candidate.name);
          killed.push(candidate);
        },
        KillOutcome::Skipped => {},
        KillOutcome::Failed => report.survivors.push(candidate.pid),
      }
    }
  }
  wait_for_exit(&mut system, killed, deadline, &mut report);
  report
}

/// Every process other than `own_pid` whose environment carries `id`.
pub(crate) fn scan_candidates(system: &mut System, id: &str, own_pid: u32) -> Scan {
  system.refresh_processes_specifics(ProcessesToUpdate::All, true, refresh_kind());
  let mut scan = Scan {
    candidates: Vec::new(),
    own_pid_seen: false,
  };
  for (pid, process) in system.processes() {
    let pid = pid.as_u32();
    if pid == own_pid {
      scan.own_pid_seen = true;
    } else if process.thread_kind().is_none()
      && process.status() != ProcessStatus::Zombie
      && carries_id(process.environ(), id)
    {
      scan.candidates.push(Candidate {
        pid,
        start_time: process.start_time(),
        name: process.name().to_string_lossy().into_owned(),
      });
    }
  }
  scan
}

/// Refresh one candidate and `SIGKILL` it only if it is still the same
/// process and still carries `id` — the guard against PID reuse and `exec`.
pub(crate) fn kill_verified(system: &mut System, id: &str, candidate: &Candidate) -> KillOutcome {
  let pid = Pid::from_u32(candidate.pid);
  system.refresh_processes_specifics(ProcessesToUpdate::Some(&[pid]), true, refresh_kind());
  let Some(process) = system.process(pid) else {
    return KillOutcome::Skipped;
  };
  if process.start_time() != candidate.start_time || !carries_id(process.environ(), id) {
    return KillOutcome::Skipped;
  }
  match process.kill_with(Signal::Kill) {
    Some(true) => KillOutcome::Killed,
    Some(false) | None => KillOutcome::Failed,
  }
}

/// Poll until every killed process is gone or a zombie, or the deadline
/// passes; survivors that vanished on their own also count as terminated.
fn wait_for_exit(
  system: &mut System,
  mut pending: Vec<Candidate>,
  deadline: Instant,
  report: &mut CleanupReport,
) {
  loop {
    pending.retain(|candidate| {
      let exited = has_exited(system, candidate.pid, Some(candidate.start_time));
      if exited {
        report.terminated.push(candidate.pid);
      }
      !exited
    });
    if pending.is_empty() {
      break;
    }
    if Instant::now() >= deadline {
      report.timed_out = true;
      report
        .survivors
        .extend(pending.iter().map(|candidate| candidate.pid));
      break;
    }
    std::thread::sleep(EXIT_POLL);
  }
  let survivors = std::mem::take(&mut report.survivors);
  for pid in survivors {
    if has_exited(system, pid, None) {
      report.terminated.push(pid);
    } else {
      report.survivors.push(pid);
    }
  }
}

/// Gone, a zombie, or (when `start_time` is known) replaced by a new process.
fn has_exited(system: &mut System, pid: u32, start_time: Option<u64>) -> bool {
  let pid = Pid::from_u32(pid);
  system.refresh_processes_specifics(ProcessesToUpdate::Some(&[pid]), true, refresh_kind());
  system.process(pid).is_none_or(|process| {
    process.status() == ProcessStatus::Zombie
      || start_time.is_some_and(|start| start != process.start_time())
  })
}

/// Environment only; per-thread entries skipped so Linux reads each process once.
fn refresh_kind() -> ProcessRefreshKind {
  ProcessRefreshKind::nothing()
    .without_tasks()
    .with_environ(UpdateKind::Always)
}

async fn warn_on_report(events: &mpsc::Sender<RunnerEvent>, report: &CleanupReport) {
  for warning in report_warnings(report) {
    tracing::warn!(?report, "{warning}");
    job_log(events, format!("##[warning]{warning}")).await;
  }
}

/// One WARN line per abnormal sweep outcome: degraded scan, deadline, survivors.
fn report_warnings(report: &CleanupReport) -> Vec<String> {
  let mut warnings = Vec::new();
  if report.degraded {
    warnings.push("Orphan process cleanup could not enumerate processes on this host.".to_owned());
  }
  if report.timed_out {
    warnings.push(format!(
      "Orphan process cleanup stopped after {}s.",
      ORPHAN_CLEANUP_TIMEOUT.as_secs()
    ));
  }
  if !report.survivors.is_empty() {
    warnings.push(format!(
      "Orphan processes still running after cleanup: {:?}",
      report.survivors
    ));
  }
  warnings
}

fn job_event(line: String) -> RunnerEvent {
  RunnerEvent::Log {
    step_id: String::new(),
    line,
    stream: LogStream::Stdout,
  }
}

async fn job_log(events: &mpsc::Sender<RunnerEvent>, line: String) {
  if events.send(job_event(line)).await.is_err() {
    tracing::warn!("event channel closed; orphan cleanup log line was dropped");
  }
}

#[cfg(test)]
#[path = "tests/orphan_cleanup.rs"]
mod tests;
