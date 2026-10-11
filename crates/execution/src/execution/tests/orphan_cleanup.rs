//! Orphan sweep internals against real processes (issue #89, AC-4/AC-5).

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::Write;
use std::os::unix::process::ExitStatusExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use shared::VariableValue;
use sysinfo::{Pid, ProcessesToUpdate, System};

use super::{
  CleanupReport, KillOutcome, ProcessTracking, carries_id, kill_verified, process_clean_enabled,
  refresh_kind, report_warnings, scan_candidates, sweep_blocking,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn variables(pairs: &[(&str, &str)]) -> HashMap<String, VariableValue> {
  pairs
    .iter()
    .map(|(key, value)| {
      (
        (*key).to_owned(),
        VariableValue {
          value: (*value).to_owned(),
          is_secret: false,
        },
      )
    })
    .collect()
}

fn fresh_id() -> String {
  format!("github_{}", uuid::Uuid::new_v4())
}

fn spawn_tagged(id: &str, program: &str, args: &[&str]) -> std::io::Result<Child> {
  Command::new(program)
    .args(args)
    .env("RUNNER_TRACKING_ID", id)
    .stdin(Stdio::piped())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .spawn()
}

/// Poll the real process until `ready` holds, bounded at five seconds.
fn wait_until(pid: u32, ready: impl Fn(&sysinfo::Process) -> bool) -> TestResult {
  let mut system = System::new();
  let target = Pid::from_u32(pid);
  for _ in 0..100 {
    system.refresh_processes_specifics(ProcessesToUpdate::Some(&[target]), true, refresh_kind());
    if system.process(target).is_some_and(&ready) {
      return Ok(());
    }
    std::thread::sleep(Duration::from_millis(50));
  }
  Err(format!("pid {pid} never reached the expected state").into())
}

/// Poll real `ps -o stat=` until `pid` is a zombie (`Z…`). Portable on purpose:
/// macOS sysinfo drops a zombie from refreshes (its `KERN_PROCARGS2` read
/// fails) instead of reporting `ProcessStatus::Zombie`.
fn wait_for_zombie(pid: u32) -> TestResult {
  for _ in 0..100 {
    let output = Command::new("ps")
      .args(["-o", "stat=", "-p", &pid.to_string()])
      .output()?;
    if String::from_utf8(output.stdout)?.trim().starts_with('Z') {
      return Ok(());
    }
    std::thread::sleep(Duration::from_millis(50));
  }
  Err(format!("pid {pid} never became a zombie").into())
}

fn far_deadline() -> Instant {
  Instant::now() + Duration::from_secs(10)
}

#[test]
fn process_clean_only_false_disables_tracking() {
  for disabled in ["false", "False", " FALSE "] {
    assert!(
      !process_clean_enabled(&variables(&[("process.clean", disabled)])),
      "{disabled:?}"
    );
  }
  assert!(!process_clean_enabled(&variables(&[(
    "Process.Clean",
    "false"
  )])));
  for enabled in ["true", "0", "no", "garbage", ""] {
    assert!(
      process_clean_enabled(&variables(&[("process.clean", enabled)])),
      "{enabled:?}"
    );
  }
  assert!(process_clean_enabled(&variables(&[])));
  assert!(ProcessTracking::for_job(&variables(&[("process.clean", "false")])).is_none());
}

#[test]
fn process_clean_enabled_jobs_get_fresh_github_ids() -> TestResult {
  let enabled = "tracking must be enabled by default";
  let first = ProcessTracking::for_job(&variables(&[])).ok_or(enabled)?;
  let second = ProcessTracking::for_job(&variables(&[])).ok_or(enabled)?;
  assert!(first.id().starts_with("github_") && first.id().len() == "github_".len() + 36);
  assert_ne!(first.id(), second.id());
  Ok(())
}

#[test]
fn carries_id_matches_exact_key_and_case_insensitive_value() {
  let id = "github_0a1b";
  let env = |entries: &[&str]| entries.iter().map(OsString::from).collect::<Vec<_>>();
  assert!(carries_id(
    &env(&["PATH=/bin", "RUNNER_TRACKING_ID=github_0a1b"]),
    id
  ));
  assert!(carries_id(&env(&["RUNNER_TRACKING_ID=GITHUB_0A1B"]), id));
  assert!(!carries_id(&env(&["RUNNER_TRACKING_ID="]), id));
  assert!(!carries_id(&env(&["RUNNER_TRACKING_ID=github_other"]), id));
  assert!(!carries_id(&env(&["RUNNER_TRACKING_ID_X=github_0a1b"]), id));
  assert!(!carries_id(&env(&["runner_tracking_id=github_0a1b"]), id));
  assert!(!carries_id(&env(&["RUNNER_TRACKING_ID"]), id));
  assert!(!carries_id(&[], id));
}

#[test]
fn sweep_kills_a_live_tagged_process() -> TestResult {
  let id = fresh_id();
  let mut child = spawn_tagged(&id, "sleep", &["300"])?;
  let pid = child.id();
  wait_until(pid, |process| carries_id(process.environ(), &id))?;
  let mut killed = Vec::new();
  let report = sweep_blocking(&id, std::process::id(), far_deadline(), |pid, name| {
    killed.push((pid, name.to_owned()));
  });
  let status = child.wait()?;
  assert_eq!(status.signal(), Some(9), "{report:?}");
  assert_eq!(killed, [(pid, "sleep".to_owned())]);
  assert_eq!(report.terminated, [pid]);
  assert!(report.survivors.is_empty() && !report.timed_out && !report.degraded);
  Ok(())
}

#[test]
fn exec_without_id_between_scan_and_kill_is_not_signalled() -> TestResult {
  let id = fresh_id();
  let mut child = spawn_tagged(&id, "sh", &["-c", "read _; exec env -i sleep 300"])?;
  let pid = child.id();
  wait_until(pid, |process| carries_id(process.environ(), &id))?;

  let mut system = System::new();
  let scan = scan_candidates(&mut system, &id, std::process::id());
  let candidate = scan
    .candidates
    .iter()
    .find(|candidate| candidate.pid == pid)
    .ok_or("tagged process not scanned")?
    .clone();

  // Same pid, new image, environment without the id: the PID-reuse stand-in.
  child.stdin.take().ok_or("stdin")?.write_all(b"go\n")?;
  wait_until(pid, |process| !carries_id(process.environ(), &id))?;

  let outcome = kill_verified(&mut system, &id, &candidate);
  let still_running = child.try_wait()?.is_none();
  child.kill()?;
  child.wait()?;
  assert_eq!(outcome, KillOutcome::Skipped);
  assert!(
    still_running,
    "re-exec'd process without the id was signalled"
  );
  Ok(())
}

#[test]
fn unreaped_zombie_carrying_id_is_skipped() -> TestResult {
  let id = fresh_id();
  let mut child = spawn_tagged(&id, "sh", &["-c", "exit 0"])?;
  let pid = child.id();
  wait_for_zombie(pid)?;
  let mut killed = Vec::new();
  let report = sweep_blocking(&id, std::process::id(), far_deadline(), |pid, _| {
    killed.push(pid);
  });
  child.wait()?;
  assert!(killed.is_empty(), "{report:?}");
  assert!(
    report.terminated.is_empty() && report.survivors.is_empty(),
    "{report:?}"
  );
  assert!(!report.timed_out && !report.degraded);
  Ok(())
}

#[test]
fn expired_deadline_kills_nothing_and_reports_timeout() -> TestResult {
  let id = fresh_id();
  let mut child = spawn_tagged(&id, "sleep", &["300"])?;
  wait_until(child.id(), |process| carries_id(process.environ(), &id))?;
  let report = sweep_blocking(&id, std::process::id(), Instant::now(), |_, _| {});
  let still_running = child.try_wait()?.is_none();
  child.kill()?;
  child.wait()?;
  assert!(report.timed_out, "{report:?}");
  assert!(report.terminated.is_empty());
  assert!(still_running);
  Ok(())
}

#[test]
fn report_warnings_name_each_abnormal_outcome_once() {
  assert!(report_warnings(&CleanupReport::default()).is_empty());
  let clean_kill = CleanupReport {
    terminated: vec![41],
    ..CleanupReport::default()
  };
  assert!(report_warnings(&clean_kill).is_empty());
  let everything = CleanupReport {
    terminated: vec![41],
    survivors: vec![42, 43],
    timed_out: true,
    degraded: true,
  };
  assert_eq!(
    report_warnings(&everything),
    [
      "Orphan process cleanup could not enumerate processes on this host.",
      "Orphan process cleanup stopped after 15s.",
      "Orphan processes still running after cleanup: [42, 43]",
    ]
  );
}
