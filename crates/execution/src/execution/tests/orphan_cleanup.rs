//! Orphan sweep internals against real processes (issue #89, AC-4/AC-5).

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use shared::VariableValue;
use sysinfo::{Pid, ProcessesToUpdate, System};

use super::{
  CleanupReport, KillOutcome, ProcessTracking, TRACKING_ENV, Tracked, carries_id, in_tracked_group,
  kill_verified, process_clean_enabled, refresh_kind, report_warnings, scan_candidates,
  sweep_blocking,
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

fn spawn_tagged(id: &str, program: &Path, args: &[&str]) -> std::io::Result<Child> {
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

/// A snapshot that matches by environment only (no tracked groups).
fn env_only(id: &str) -> Tracked {
  Tracked {
    id: id.to_owned(),
    started: 0,
    groups: Vec::new(),
  }
}

/// `sleep` and a POSIX shell whose environment the kernel shows other
/// processes. With SIP on, macOS hides the environment of Apple platform
/// binaries (`cs_restricted`), so the env-path tests run ad-hoc re-signed
/// copies of `/bin/sleep` and `/bin/bash` there (`/bin/sh` is a shim that
/// re-execs a restricted shell). Linux reads `/proc` for every binary.
struct VisibleTools {
  _dir: tempfile::TempDir,
  sleep: PathBuf,
  shell: PathBuf,
}

fn visible_tools() -> Result<VisibleTools, Box<dyn std::error::Error>> {
  let dir = tempfile::tempdir()?;
  if !cfg!(target_os = "macos") {
    return Ok(VisibleTools {
      _dir: dir,
      sleep: PathBuf::from("sleep"),
      shell: PathBuf::from("/bin/sh"),
    });
  }
  let resign = |source: &str, name: &str| -> Result<PathBuf, Box<dyn std::error::Error>> {
    let copy = dir.path().join(name);
    std::fs::copy(source, &copy)?;
    let status = Command::new("/usr/bin/codesign")
      .args(["-f", "-s", "-"])
      .arg(&copy)
      .stderr(Stdio::null())
      .status()?;
    if !status.success() {
      return Err(format!("codesign could not ad-hoc sign {}", copy.display()).into());
    }
    Ok(copy)
  };
  let sleep = resign("/bin/sleep", "sleep")?;
  let shell = resign("/bin/bash", "bash")?;
  Ok(VisibleTools {
    _dir: dir,
    sleep,
    shell,
  })
}

/// `ps -o stat=` for `pid`: `None` once it is gone.
fn ps_state(pid: u32) -> Result<Option<String>, Box<dyn std::error::Error>> {
  let output = Command::new("ps")
    .args(["-o", "stat=", "-p", &pid.to_string()])
    .output()?;
  let state = String::from_utf8(output.stdout)?.trim().to_owned();
  Ok((!state.is_empty()).then_some(state))
}

fn running(pid: u32) -> Result<bool, Box<dyn std::error::Error>> {
  Ok(ps_state(pid)?.is_some_and(|state| !state.starts_with('Z')))
}

fn force_kill(pid: u32) {
  // Best-effort cleanup of a process this test started; it may be gone.
  if let Err(error) = Command::new("kill").args(["-9", &pid.to_string()]).status() {
    eprintln!("cleanup kill of {pid} failed: {error}");
  }
}

/// A `/bin/sh` step root leading its own process group (what
/// `spawn_in_cgroup` does), carrying `id`; it runs `script` and its first
/// stdout line is returned with the root's pid once the root has exited.
fn group_root(
  id: &str,
  script: &str,
  args: &[&str],
) -> Result<(Command, u32, String), Box<dyn std::error::Error>> {
  let mut command = Command::new("/bin/sh");
  command
    .arg("-c")
    .arg(script)
    .arg("sh")
    .args(args)
    .env(TRACKING_ENV, id)
    .process_group(0)
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::null());
  let mut child = command.spawn()?;
  let root = child.id();
  let mut line = String::new();
  BufReader::new(child.stdout.take().ok_or("stdout")?).read_line(&mut line)?;
  child.wait()?;
  Ok((command, root, line.trim().to_owned()))
}

fn wait_for_path(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
  for _ in 0..200 {
    if path.exists() {
      return Ok(());
    }
    std::thread::sleep(Duration::from_millis(50));
  }
  Err(format!("{} never appeared", path.display()).into())
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
  let tools = visible_tools()?;
  let id = fresh_id();
  let mut child = spawn_tagged(&id, &tools.sleep, &["300"])?;
  let pid = child.id();
  wait_until(pid, |process| carries_id(process.environ(), &id))?;
  let mut killed = Vec::new();
  let report = sweep_blocking(
    &env_only(&id),
    std::process::id(),
    far_deadline(),
    |pid, name| {
      killed.push((pid, name.to_owned()));
    },
  );
  let status = child.wait()?;
  assert_eq!(status.signal(), Some(9), "{report:?}");
  assert_eq!(killed, [(pid, "sleep".to_owned())]);
  assert_eq!(report.terminated, [pid]);
  assert!(report.survivors.is_empty() && !report.timed_out && !report.degraded && !report.failed);
  Ok(())
}

#[test]
fn exec_without_id_between_scan_and_kill_is_not_signalled() -> TestResult {
  let tools = visible_tools()?;
  let id = fresh_id();
  // One exec, straight to the id-less image (a two-step `exec env -i sleep`
  // passes through an `env` image that still carries the id). The exported
  // marker exists only in that final image's exec-time environment; the
  // re-signed sleep is exec'd by path so that image stays readable.
  let sleep = tools.sleep.to_string_lossy().into_owned();
  let mut child = spawn_tagged(
    &id,
    &tools.shell,
    &[
      "-c",
      "read _; unset RUNNER_TRACKING_ID; export ORPHAN89_EXECED=1; exec \"$0\" 300",
      &sleep,
    ],
  )?;
  let pid = child.id();
  wait_until(pid, |process| carries_id(process.environ(), &id))?;

  let tracked = env_only(&id);
  let mut system = System::new();
  let scan = scan_candidates(&mut system, &tracked, std::process::id());
  let candidate = scan
    .candidates
    .iter()
    .find(|candidate| candidate.pid == pid)
    .ok_or("tagged process not scanned")?
    .clone();

  // Same pid, new image, environment without the id: the PID-reuse stand-in.
  // Wait for that final state only — the marker present and the id gone —
  // never a transient empty read taken mid-exec. (sysinfo keeps a process's
  // first-seen name, so the image name cannot be used to detect the exec.)
  child.stdin.take().ok_or("stdin")?.write_all(b"go\n")?;
  wait_until(pid, |process| {
    process
      .environ()
      .iter()
      .any(|entry| entry.as_encoded_bytes() == b"ORPHAN89_EXECED=1")
      && !carries_id(process.environ(), &id)
  })?;

  let outcome = kill_verified(&mut system, &tracked, &candidate);
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
  let mut child = spawn_tagged(&id, Path::new("sh"), &["-c", "exit 0"])?;
  let pid = child.id();
  wait_for_zombie(pid)?;
  let mut killed = Vec::new();
  let report = sweep_blocking(
    &env_only(&id),
    std::process::id(),
    far_deadline(),
    |pid, _| {
      killed.push(pid);
    },
  );
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
  let tools = visible_tools()?;
  let id = fresh_id();
  let mut child = spawn_tagged(&id, &tools.sleep, &["300"])?;
  wait_until(child.id(), |process| carries_id(process.environ(), &id))?;
  let report = sweep_blocking(
    &env_only(&id),
    std::process::id(),
    Instant::now(),
    |_, _| {},
  );
  let still_running = child.try_wait()?.is_none();
  child.kill()?;
  child.wait()?;
  assert!(report.timed_out, "{report:?}");
  assert!(report.terminated.is_empty());
  assert!(still_running);
  Ok(())
}

#[test]
fn register_group_needs_the_jobs_own_id_in_the_spawn_env() -> TestResult {
  let tracking = ProcessTracking::for_job(&variables(&[])).ok_or("tracking enabled")?;
  let mut opted_out = Command::new("true");
  opted_out.env(TRACKING_ENV, "");
  tracking.register_group(&opted_out, 101);
  let mut other = Command::new("true");
  other.env(TRACKING_ENV, "github_someone-else");
  tracking.register_group(&other, 102);
  tracking.register_group(&Command::new("true"), 103);
  let mut tracked = Command::new("true");
  tracked.env(TRACKING_ENV, tracking.id().to_ascii_uppercase());
  tracking.register_group(&tracked, 104);
  let ids: Vec<u32> = tracking
    .snapshot()
    .groups
    .iter()
    .map(|group| group.id)
    .collect();
  assert_eq!(ids, [104]);
  Ok(())
}

#[test]
fn group_member_without_the_id_is_killed() -> TestResult {
  let tracking = ProcessTracking::for_job(&variables(&[])).ok_or("tracking enabled")?;
  // The root exits right after the echo, leaving an id-less `sleep` that only
  // its process group ties to the job.
  let script = "env -u RUNNER_TRACKING_ID sleep 300 >/dev/null 2>&1 & echo $!";
  let (command, root, member) = group_root(tracking.id(), script, &[])?;
  tracking.register_group(&command, root);
  let (_, _, twin) = group_root(tracking.id(), script, &[])?;
  let (member, twin): (u32, u32) = (member.parse()?, twin.parse()?);

  let report = sweep_blocking(
    &tracking.snapshot(),
    std::process::id(),
    far_deadline(),
    |_, _| {},
  );
  let twin_alive = running(twin)?;
  force_kill(twin);
  force_kill(member);
  assert!(report.terminated.contains(&member), "{report:?}");
  assert!(!report.terminated.contains(&twin), "{report:?}");
  assert!(twin_alive, "an unregistered group's member was killed");
  Ok(())
}

#[test]
fn group_member_leaving_before_kill_is_skipped() -> TestResult {
  let tracking = ProcessTracking::for_job(&variables(&[])).ok_or("tracking enabled")?;
  let dir = tempfile::tempdir()?;
  let trigger = dir.path().join("go");
  let left = dir.path().join("left");
  // A non-leader member (a group leader cannot leave its group) that moves to
  // a group of its own once the trigger appears.
  let helper = "import os,sys,time\n\
while not os.path.exists(sys.argv[1]): time.sleep(0.05)\n\
os.setpgid(0, 0)\n\
open(sys.argv[2], 'w').close()\n\
time.sleep(300)\n";
  // `env -u`: without the id only the group can tie it to the job, on every OS.
  let script =
    "env -u RUNNER_TRACKING_ID python3 -c \"$1\" \"$2\" \"$3\" >/dev/null 2>&1 & echo $!";
  let (command, root, member) = group_root(
    tracking.id(),
    script,
    &[helper, &trigger.to_string_lossy(), &left.to_string_lossy()],
  )?;
  tracking.register_group(&command, root);
  let member: u32 = member.parse()?;

  let tracked = tracking.snapshot();
  let mut system = System::new();
  let candidate = scan_candidates(&mut system, &tracked, std::process::id())
    .candidates
    .into_iter()
    .find(|candidate| candidate.pid == member)
    .ok_or("group member not scanned")?;
  std::fs::write(&trigger, "go")?;
  wait_for_path(&left)?;

  let outcome = kill_verified(&mut system, &tracked, &candidate);
  let still_running = running(member)?;
  force_kill(member);
  assert_eq!(outcome, KillOutcome::Skipped);
  assert!(still_running, "a process that left the group was signalled");
  Ok(())
}

/// A `/bin/sh` group leader that stays alive (it execs into `sleep`) with a
/// background member, neither ever carrying a job id — only the group can
/// tie them to a job; returns the leader and the member's pid. (A leader
/// spawned with the id could be swept by its environment before its `exec`,
/// and a killed, unreaped leader then reads as gone.)
fn live_group() -> Result<(Child, u32), Box<dyn std::error::Error>> {
  let mut leader = Command::new("/bin/sh")
    .args(["-c", "sleep 300 >/dev/null 2>&1 & echo $!; exec sleep 301"])
    .env_remove(TRACKING_ENV)
    .process_group(0)
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::null())
    .spawn()?;
  let mut line = String::new();
  BufReader::new(leader.stdout.take().ok_or("stdout")?).read_line(&mut line)?;
  Ok((leader, line.trim().parse()?))
}

#[test]
fn a_group_whose_id_now_names_another_leader_is_spared() -> TestResult {
  let tracking = ProcessTracking::for_job(&variables(&[])).ok_or("tracking enabled")?;
  let (mut leader, member) = live_group()?;
  let mut command = Command::new("true");
  command.env(TRACKING_ENV, tracking.id());
  tracking.register_group(&command, leader.id());
  // The same group id, but recorded with a leader that started at another
  // time: the id now belongs to an unrelated group.
  let mut recycled = tracking.snapshot();
  for group in &mut recycled.groups {
    group.leader_start = Some(1);
  }
  let spared = sweep_blocking(&recycled, std::process::id(), far_deadline(), |_, _| {});
  let member_alive = running(member)?;
  let owned = sweep_blocking(
    &tracking.snapshot(),
    std::process::id(),
    far_deadline(),
    |_, _| {},
  );
  force_kill(member);
  leader.kill()?;
  leader.wait()?;
  assert!(!spared.terminated.contains(&member), "{spared:?}");
  assert!(member_alive, "a member of a recycled group id was killed");
  assert!(owned.terminated.contains(&member), "{owned:?}");
  Ok(())
}

#[test]
fn group_matches_need_a_start_since_the_job_and_the_runners_uid() -> TestResult {
  let tracking = ProcessTracking::for_job(&variables(&[])).ok_or("tracking enabled")?;
  let (command, root, member) = group_root(
    tracking.id(),
    "env -u RUNNER_TRACKING_ID sleep 300 >/dev/null 2>&1 & echo $!",
    &[],
  )?;
  tracking.register_group(&command, root);
  let member: u32 = member.parse()?;
  let tracked = tracking.snapshot();

  let mut later = tracked.clone();
  later.started += 3600;
  let report = sweep_blocking(&later, std::process::id(), far_deadline(), |_, _| {});
  let alive_after_later = running(member)?;

  let mut system = System::new();
  let pid = Pid::from_u32(member);
  system.refresh_processes_specifics(
    ProcessesToUpdate::Some(&[pid]),
    true,
    sysinfo::ProcessRefreshKind::nothing().with_user(sysinfo::UpdateKind::Always),
  );
  let process = system.process(pid).ok_or("member not visible")?;
  let own = process.user_id().cloned().ok_or("member uid unreadable")?;
  let other: sysinfo::Uid = if own.to_string() == "0" { "1" } else { "0" }.parse()?;
  let matches_own = in_tracked_group(&system, process, &tracked, Some(&own));
  let matches_other = in_tracked_group(&system, process, &tracked, Some(&other));
  let matches_unknown = in_tracked_group(&system, process, &tracked, None);
  force_kill(member);

  assert!(report.terminated.is_empty(), "{report:?}");
  assert!(
    alive_after_later,
    "a process older than the job was matched"
  );
  assert!(matches_own);
  assert!(!matches_other, "another user's process was matched");
  assert!(!matches_unknown, "an unknown runner uid matched");
  Ok(())
}

#[test]
fn release_forgets_only_groups_left_empty() -> TestResult {
  let tracking = ProcessTracking::for_job(&variables(&[])).ok_or("tracking enabled")?;
  let (empty_command, empty_root, _) = group_root(tracking.id(), "echo done", &[])?;
  tracking.register_group(&empty_command, empty_root);
  let (busy_command, busy_root, member) =
    group_root(tracking.id(), "sleep 300 >/dev/null 2>&1 & echo $!", &[])?;
  tracking.register_group(&busy_command, busy_root);
  tracking.release_if_empty(empty_root);
  tracking.release_if_empty(busy_root);
  let ids: Vec<u32> = tracking
    .snapshot()
    .groups
    .iter()
    .map(|group| group.id)
    .collect();
  force_kill(member.parse()?);
  assert_eq!(ids, [busy_root]);
  Ok(())
}

/// Run `script` as a host `run:` step through the production `ScriptHandler`,
/// tracked by `tracking`.
async fn run_tracked_step(tracking: &ProcessTracking, script: &str) -> TestResult {
  let dir = tempfile::tempdir()?;
  let env = HashMap::from([("PATH".to_owned(), std::env::var("PATH")?)]);
  let cancel = tokio_util::sync::CancellationToken::new();
  let (events, _events_rx) = tokio::sync::mpsc::channel(256);
  let (stdout, mut lines) = tokio::sync::mpsc::channel(256);
  let output = crate::execution::handlers::script::ScriptHandler::new()
    .execute(
      &crate::execution::handlers::script::ScriptParams {
        script,
        shell: Some("bash"),
        env: &env,
        working_dir: dir.path(),
        step_id: "orphan-group",
        cgroup_path: None,
        tracking_id: Some(tracking.id()),
        tracking: Some(tracking),
        timeout: Some(Duration::from_secs(15)),
        cancel: &cancel,
        container: None,
        report_metadata: false,
      },
      &events,
      stdout,
    )
    .await?;
  while lines.recv().await.is_some() {}
  assert_eq!(output.conclusion, shared::Conclusion::Success);
  Ok(())
}

#[tokio::test]
async fn the_script_handler_releases_a_step_group_only_once_it_is_empty() -> TestResult {
  let tracking = ProcessTracking::for_job(&variables(&[])).ok_or("tracking enabled")?;
  run_tracked_step(&tracking, "true").await?;
  assert!(
    tracking.snapshot().groups.is_empty(),
    "a step that left nothing kept its group registered"
  );

  let dir = tempfile::tempdir()?;
  let pid_file = dir.path().join("member");
  run_tracked_step(
    &tracking,
    &format!(
      "sleep 300 >/dev/null 2>&1 & echo $! > '{}'",
      pid_file.display()
    ),
  )
  .await?;
  let groups = tracking.snapshot().groups;
  force_kill(std::fs::read_to_string(&pid_file)?.trim().parse()?);
  assert_eq!(
    groups.len(),
    1,
    "a step with a live member lost its group: {groups:?}"
  );
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
    failed: true,
  };
  assert_eq!(
    report_warnings(&everything),
    [
      "Orphan process cleanup did not complete; see the runner diagnostics log.",
      "Orphan process cleanup could not enumerate processes on this host.",
      "Orphan process cleanup stopped after 15s.",
      "Orphan processes still running after cleanup: [42, 43]",
    ]
  );
}
