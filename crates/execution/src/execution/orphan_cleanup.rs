//! Job-end orphan process cleanup keyed on `RUNNER_TRACKING_ID` (issue #89).
//!
//! Mirrors actions/runner `JobExtension` (pinned `cab9d1c`, lines 586-600 and
//! 880-925): every host step process inherits a fresh per-job
//! `RUNNER_TRACKING_ID=github_<uuid>`, and when the job finishes every other
//! process whose environment still carries that id receives `SIGKILL`.
//! Linux reads `/proc/<pid>/environ`; macOS reads `sysctl(KERN_PROCARGS2)`.
//! Both expose the environment captured at `exec` and only for processes the
//! runner's user may inspect, so `sudo` and other users' processes fall
//! outside the sweep, exactly as upstream.
//!
//! With SIP on, macOS returns no environment at all for `cs_restricted`
//! targets — Apple platform binaries such as `sleep`, `sh`, `bash`, `zsh` and
//! `/usr/bin/python3` — even to their parent. A second, inheritance-based
//! match covers them: every host step leads its own process group
//! (`spawn_in_cgroup`), the step's group is registered with the job's
//! tracking when its spawn environment carries the job's id
//! ([`ProcessTracking::register_group`]), and a same-user process started
//! during the job whose process group is one of those is swept too. A group
//! is forgotten once its step leaves it empty
//! ([`ProcessTracking::release_if_empty`]), and one whose id now names a live
//! leader with another start time is treated as reused, so an unrelated group
//! that inherits a recycled id is not matched. One narrow case remains: a
//! group still registered because its members outlived its step, whose
//! members all died, whose id the kernel then reused for a new same-user
//! group, and whose new leader already exited leaving members — those match.
//! A daemon that leaves the
//! group (`setsid`/`setpgid`) is still found only by its environment, so a
//! platform-binary daemon on SIP macOS is not covered.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::process::Command;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use shared::{LogStream, RunnerEvent, VariableValue};
use sysinfo::{
  Pid, Process, ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, Signal, System, Uid,
  UpdateKind,
};
use tokio::sync::mpsc;

/// Environment variable carrying the per-job tracking id.
pub(crate) const TRACKING_ENV: &str = "RUNNER_TRACKING_ID";
/// Job message variable that disables tracking when it parses as `false`.
const PROCESS_CLEAN_VARIABLE: &str = "process.clean";
/// Shared deadline for every scan, kill and exit poll of one sweep.
pub(crate) const ORPHAN_CLEANUP_TIMEOUT: Duration = Duration::from_secs(15);
/// Liveness backstop for a blocking scan stuck inside one `/proc` read.
///
/// The cooperative `deadline` stops the sweep at its next check; this bound
/// only matters when a single syscall never returns (a D-state target). No
/// design can reclaim that blocking thread, so the trade-off is deliberate:
/// finalization stops waiting after the backstop and the job completes, while
/// the parked thread waits for the kernel. The thread holds no event sender
/// (kills go into a shared list that `sweep` drains once, after the join or
/// the backstop, so every kill made by then is logged on every outcome), so it
/// cannot keep the job's event channel — and with it job completion — open.
const JOIN_BACKSTOP: Duration = Duration::from_secs(2);
/// Interval between exit polls of killed processes.
const EXIT_POLL: Duration = Duration::from_millis(100);
/// Job log header, upstream's wording.
const HEADER: &str = "Cleaning up orphan processes";
/// Seconds a group match may appear to predate the job: Linux derives a
/// process start time from whole-second boot time plus clock ticks, so a
/// process started in the job's first second can read up to a second early.
const START_SLACK_SECS: u64 = 2;

/// The tracking of one job; `None` on the context when `process.clean=false`.
#[derive(Debug, Clone)]
pub struct ProcessTracking {
  id: String,
  /// Unix seconds at job entry: a group match must have started since.
  started: u64,
  /// Process groups led by host steps whose spawn environment carried `id`.
  groups: Arc<Mutex<Vec<TrackedGroup>>>,
}

/// One registered step process group: its id (the step root's pid) and that
/// leader's start time, so a later group reusing the id is told apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TrackedGroup {
  /// The process-group id, equal to the step root's pid.
  pub(crate) id: u32,
  /// The leader's start time (seconds since the epoch) when registered.
  pub(crate) leader_start: Option<u64>,
}

impl ProcessTracking {
  /// A fresh `github_<uuid v4>` id unless the job's `process.clean` is `false`.
  pub(crate) fn for_job(variables: &HashMap<String, VariableValue>) -> Option<Self> {
    process_clean_enabled(variables).then(|| Self {
      id: format!("github_{}", uuid::Uuid::new_v4()),
      started: SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs()),
      groups: Arc::default(),
    })
  }

  /// The id written into host step process environments.
  pub fn id(&self) -> &str {
    &self.id
  }

  /// Record `group` — the pid of a step root spawned as its own process
  /// group leader — when `command` set `RUNNER_TRACKING_ID` to this job's id
  /// (ASCII case-insensitive). A step that opted out (`''`) or overrode the
  /// id is never group-tracked, preserving upstream's opt-out.
  pub fn register_group(&self, command: &Command, group: u32) {
    let tracked = command.get_envs().any(|(key, value)| {
      key == TRACKING_ENV
        && value.is_some_and(|value| {
          value
            .as_encoded_bytes()
            .eq_ignore_ascii_case(self.id.as_bytes())
        })
    });
    if tracked {
      let leader_start = start_time_of(group);
      self
        .groups
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(TrackedGroup {
          id: group,
          leader_start,
        });
    }
  }

  /// Forget `group` once no process is left in it (its step finished
  /// without leaving anything behind), so a later, unrelated group that
  /// reuses the id can never match. A group that still has members — a
  /// background service a later step uses — stays tracked.
  pub fn release_if_empty(&self, group: u32) {
    if group_is_empty(group) {
      self
        .groups
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .retain(|tracked| tracked.id != group);
    }
  }

  /// What one sweep matches against, frozen when the sweep starts.
  pub(crate) fn snapshot(&self) -> Tracked {
    Tracked {
      id: self.id.clone(),
      started: self.started,
      groups: self
        .groups
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone(),
    }
  }
}

/// A job's tracking as one sweep sees it.
#[derive(Debug, Clone)]
pub(crate) struct Tracked {
  /// The job's `RUNNER_TRACKING_ID`.
  pub(crate) id: String,
  /// Unix seconds at job entry.
  pub(crate) started: u64,
  /// Registered step process groups.
  pub(crate) groups: Vec<TrackedGroup>,
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
  /// The blocking sweep panicked or was cancelled before reporting.
  pub failed: bool,
}

/// How a candidate is tied to the job; re-verified by the same evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Evidence {
  /// Its environment carries the job's id.
  Environment,
  /// It sits in one of the job's step process groups.
  Group,
}

/// One process found tied to the job during a scan.
#[derive(Debug, Clone)]
pub(crate) struct Candidate {
  pub(crate) pid: u32,
  start_time: u64,
  name: String,
  evidence: Evidence,
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
  /// Gone, re-exec'd without the id, moved out of its step group, or a
  /// different process now: untouched.
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
  let tracked = tracking.snapshot();
  let own_pid = std::process::id();
  let deadline = Instant::now() + ORPHAN_CLEANUP_TIMEOUT;
  // The blocking closure owns no event sender: a thread parked in the kernel
  // must never hold the job's event channel open past the backstop. Kills are
  // shared through a plain list instead, so every arm below can log them.
  let kills = Arc::new(Mutex::new(Vec::new()));
  let sink = Arc::clone(&kills);
  let task = tokio::task::spawn_blocking(move || {
    sweep_blocking(&tracked, own_pid, deadline, |pid, name| {
      sink
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push((pid, name.to_owned()));
    })
  });
  let joined = tokio::time::timeout(ORPHAN_CLEANUP_TIMEOUT + JOIN_BACKSTOP, task).await;
  let killed = std::mem::take(&mut *kills.lock().unwrap_or_else(PoisonError::into_inner));
  for (pid, name) in killed {
    job_log(
      events,
      format!("Terminate orphan process: pid ({pid}) ({name})"),
    )
    .await;
  }
  let report = match joined {
    Ok(Ok(report)) => report,
    Ok(Err(error)) => {
      tracing::warn!(%error, "orphan cleanup task failed");
      CleanupReport {
        failed: true,
        ..CleanupReport::default()
      }
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
  tracked: &Tracked,
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
    let scan = scan_candidates(&mut system, tracked, own_pid);
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
      match kill_verified(&mut system, tracked, &candidate) {
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

/// The start time of `pid` (seconds since the epoch); `None` once it is gone.
fn start_time_of(pid: u32) -> Option<u64> {
  let pid = Pid::from_u32(pid);
  let mut system = System::new();
  system.refresh_processes_specifics(
    ProcessesToUpdate::Some(&[pid]),
    true,
    ProcessRefreshKind::nothing(),
  );
  system.process(pid).map(Process::start_time)
}

/// The process group of `pid`; `None` once it is gone.
#[cfg(unix)]
pub(crate) fn process_group(pid: u32) -> Option<u32> {
  let raw = i32::try_from(pid).ok()?;
  let group = nix::unistd::getpgid(Some(nix::unistd::Pid::from_raw(raw))).ok()?;
  u32::try_from(group.as_raw()).ok()
}

#[cfg(not(unix))]
pub(crate) fn process_group(_pid: u32) -> Option<u32> {
  None
}

/// No process is left in `group` (signal 0 reports `ESRCH`). A group whose
/// only members belong to another user (`EPERM`) is not empty.
#[cfg(unix)]
fn group_is_empty(group: u32) -> bool {
  i32::try_from(group).is_ok_and(|raw| {
    nix::sys::signal::killpg(nix::unistd::Pid::from_raw(raw), None) == Err(nix::errno::Errno::ESRCH)
  })
}

#[cfg(not(unix))]
fn group_is_empty(_group: u32) -> bool {
  false
}

/// The registered group `id` is still this job's: its leader is gone (the
/// step root exited) or is the very process registered. A live leader with
/// another start time means the id was reused by an unrelated group.
fn group_owned(system: &System, group: &TrackedGroup) -> bool {
  system
    .process(Pid::from_u32(group.id))
    .is_none_or(|leader| group.leader_start == Some(leader.start_time()))
}

/// The registered group `process` sits in, if it is still this job's.
fn owned_group_of<'a>(system: &System, pid: u32, tracked: &'a Tracked) -> Option<&'a TrackedGroup> {
  let id = process_group(pid)?;
  tracked
    .groups
    .iter()
    .find(|group| group.id == id && group_owned(system, group))
}

/// A same-user process started during the job inside one of its step groups.
pub(crate) fn in_tracked_group(
  system: &System,
  process: &Process,
  tracked: &Tracked,
  own_uid: Option<&Uid>,
) -> bool {
  !tracked.groups.is_empty()
    && process.start_time().saturating_add(START_SLACK_SECS) >= tracked.started
    && own_uid.is_some_and(|uid| process.user_id() == Some(uid))
    && owned_group_of(system, process.pid().as_u32(), tracked).is_some()
}

/// Every process other than `own_pid` tied to the job: its environment
/// carries the id, or it sits in one of the job's step process groups.
pub(crate) fn scan_candidates(system: &mut System, tracked: &Tracked, own_pid: u32) -> Scan {
  // Owners are only needed for the group match: skip that read without groups.
  let kind = if tracked.groups.is_empty() {
    refresh_kind()
  } else {
    refresh_kind().with_user(UpdateKind::OnlyIfNotSet)
  };
  system.refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
  let runner_uid = system
    .process(Pid::from_u32(own_pid))
    .and_then(Process::user_id)
    .cloned();
  let mut scan = Scan {
    candidates: Vec::new(),
    own_pid_seen: false,
  };
  for (pid, process) in system.processes() {
    let pid = pid.as_u32();
    if pid == own_pid {
      scan.own_pid_seen = true;
      continue;
    }
    if process.thread_kind().is_some() || process.status() == ProcessStatus::Zombie {
      continue;
    }
    let evidence = if carries_id(process.environ(), &tracked.id) {
      Evidence::Environment
    } else if in_tracked_group(system, process, tracked, runner_uid.as_ref()) {
      Evidence::Group
    } else {
      continue;
    };
    scan.candidates.push(Candidate {
      pid,
      start_time: process.start_time(),
      name: process.name().to_string_lossy().into_owned(),
      evidence,
    });
  }
  scan
}

/// Refresh one candidate and `SIGKILL` it only if it is still the same
/// process and still tied to the job by the same evidence (the id in its
/// environment, or membership of a step group) — the guard against PID
/// reuse, `exec` and leaving the group.
pub(crate) fn kill_verified(
  system: &mut System,
  tracked: &Tracked,
  candidate: &Candidate,
) -> KillOutcome {
  let pid = Pid::from_u32(candidate.pid);
  // Refresh the candidate and, for a group match, its group's leader too.
  let mut targets = vec![pid];
  if candidate.evidence == Evidence::Group {
    targets.extend(process_group(candidate.pid).map(Pid::from_u32));
  }
  system.refresh_processes_specifics(ProcessesToUpdate::Some(&targets), true, refresh_kind());
  let Some(process) = system.process(pid) else {
    return KillOutcome::Skipped;
  };
  let still_tied = match candidate.evidence {
    Evidence::Environment => carries_id(process.environ(), &tracked.id),
    Evidence::Group => owned_group_of(system, candidate.pid, tracked).is_some(),
  };
  if process.start_time() != candidate.start_time || !still_tied {
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
    // Never sleep past the deadline: the last poll lands on it, not after it.
    std::thread::sleep(EXIT_POLL.min(deadline.saturating_duration_since(Instant::now())));
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

/// One WARN line per abnormal sweep outcome: failed sweep, degraded scan,
/// deadline, survivors.
fn report_warnings(report: &CleanupReport) -> Vec<String> {
  let mut warnings = Vec::new();
  if report.failed {
    warnings
      .push("Orphan process cleanup did not complete; see the runner diagnostics log.".to_owned());
  }
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
  // `send` waits while the channel is full, so its only error is a closed channel.
  if let Err(error) = events.send(job_event(line)).await {
    tracing::warn!(%error, "orphan cleanup log line was dropped");
  }
}

#[cfg(test)]
#[path = "tests/orphan_cleanup.rs"]
mod tests;
