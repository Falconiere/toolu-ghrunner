//! Issue #89: captured GitHub job replays proving the per-job
//! `RUNNER_TRACKING_ID` and the job-end orphan process cleanup.
//!
//! Every case replays the sanitized #68 GitHub.com acquisition through the
//! production `Runner` path with real Bash, Node, python3 and `ps`. Liveness is
//! observed with `ps -o stat= -p <pid>`: no row or a `Z` state means the
//! process is gone (orphans re-parent to init, which reaps them).

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use shared::{Conclusion, RunnerEvent};

#[path = "helpers/orphan_replay.rs"]
mod orphan_replay;

use orphan_replay::{
  HEADER, ORPHANS, PROBE, Replay, TestResult, assert_alive, assert_dead, assert_orphans_cleaned,
  detached_script, is_tracking_id, kill, mapping, orphan_script, ps_state, script_and_node_job,
  script_job, variable, wait_for_file,
};

// ---------------------------------------------------------------------------
// AC-1: per-job tracking id reaches every host process stage
// ---------------------------------------------------------------------------

async fn tracking_markers(replay: &Replay) -> TestResult<Vec<(String, String)>> {
  let hook = replay.pids.join("hook.sh");
  std::fs::write(
    &hook,
    "printf 'TRACK89|hook|%s\\n' \"${RUNNER_TRACKING_ID-unset}\"\n",
  )?;
  let mut job = script_and_node_job(PROBE, &[])?;
  variable(
    &mut job,
    "ACTIONS_RUNNER_HOOK_JOB_STARTED",
    &hook.to_string_lossy(),
  );
  replay.install_node_action(&job)?;
  let finished = replay.run(job, |_, _, _| {}).await?;
  assert_eq!(finished.conclusion(), Some(Conclusion::Success), "{:?}", finished.lines());
  Ok(finished.markers())
}

#[tokio::test]
async fn tracking_id_reaches_hook_shell_and_node_stages() -> TestResult {
  let replay = Replay::new().await?;
  let first = tracking_markers(&replay).await?;
  let stages: Vec<&str> = first.iter().map(|(stage, _)| stage.as_str()).collect();
  assert_eq!(stages, ["hook", "run", "node-main", "node-post"], "{first:?}");
  let id = &first.first().ok_or("no marker")?.1;
  assert!(is_tracking_id(id), "not a github_<uuid> id: {id}");
  assert!(first.iter().all(|(_, value)| value == id), "{first:?}");

  let second = tracking_markers(&Replay::new().await?).await?;
  let other = &second.first().ok_or("no marker")?.1;
  assert!(is_tracking_id(other));
  assert_ne!(id, other, "each job must get a fresh tracking id");
  Ok(())
}

/// Re-exec this test binary with an outer runner's id in its environment.
#[tokio::test]
async fn tracking_id_replaces_inherited_outer_value() -> TestResult {
  let output = tokio::process::Command::new(std::env::current_exe()?)
    .args(["--exact", "inherited_outer_id_child", "--ignored", "--nocapture"])
    .env("RUNNER_TRACKING_ID", "github_outer89")
    .env("ORPHAN89_CHILD", "1")
    .output()
    .await?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  assert!(
    output.status.success() && stdout.contains("1 passed"),
    "child failed:\n{stdout}\n{}",
    String::from_utf8_lossy(&output.stderr)
  );
  Ok(())
}

/// Child half of `tracking_id_replaces_inherited_outer_value`; only meaningful
/// with `RUNNER_TRACKING_ID=github_outer89` and `ORPHAN89_CHILD=1` set.
#[tokio::test]
#[ignore = "re-executed by tracking_id_replaces_inherited_outer_value"]
async fn inherited_outer_id_child() -> TestResult {
  if std::env::var("ORPHAN89_CHILD").is_err() {
    return Ok(());
  }
  assert_eq!(std::env::var("RUNNER_TRACKING_ID")?, "github_outer89");
  let replay = Replay::new().await?;
  let enabled = replay.run(script_job(PROBE)?, |_, _, _| {}).await?;
  let value = &enabled.markers().first().ok_or("no marker")?.1.clone();
  assert!(is_tracking_id(value) && value != "github_outer89", "{value}");

  let mut disabled = script_job(PROBE)?;
  variable(&mut disabled, "process.clean", "false");
  let disabled = replay.run(disabled, |_, _, _| {}).await?;
  assert_eq!(
    disabled.markers(),
    [("run".to_owned(), "github_outer89".to_owned())],
    "process.clean=false must pass the inherited value through"
  );
  Ok(())
}

// ---------------------------------------------------------------------------
// AC-2: every orphan kind is killed after success, failure and cancellation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn orphans_are_killed_after_successful_job() -> TestResult {
  let replay = Replay::new().await?;
  let job = script_job(&orphan_script(&replay.pids_dir(), "exit 0"))?;
  let finished = replay.run(job, |_, _, _| {}).await?;
  assert_eq!(finished.conclusion(), Some(Conclusion::Success));
  assert_orphans_cleaned(&replay, &finished, &ORPHANS).await
}

#[tokio::test]
async fn orphans_are_killed_after_failed_job() -> TestResult {
  let replay = Replay::new().await?;
  let job = script_job(&orphan_script(&replay.pids_dir(), "exit 1"))?;
  let finished = replay.run(job, |_, _, _| {}).await?;
  assert_eq!(finished.conclusion(), Some(Conclusion::Failure));
  assert_orphans_cleaned(&replay, &finished, &ORPHANS).await
}

#[tokio::test]
async fn orphans_are_killed_after_cancelled_job() -> TestResult {
  let replay = Replay::new().await?;
  let tail = r#"touch "$P/ready"; sleep 600"#;
  let job = script_job(&orphan_script(&replay.pids_dir(), tail))?;
  let ready = replay.pids.join("ready");
  let watcher = {
    let ready = ready.clone();
    tokio::spawn(async move { wait_for_file_exists(&ready).await })
  };
  let mut fired = false;
  let finished = replay
    .run(job, |event, cancel, _| {
      // Cancel once the step reports its orphans are in place.
      if !fired && matches!(event, RunnerEvent::Log { line, .. } if line.starts_with("TRACK89|"))
      {
        fired = true;
        let cancel = cancel.clone();
        let ready = ready.clone();
        tokio::spawn(async move {
          if wait_for_file_exists(&ready).await.is_ok() {
            cancel.cancel();
          }
        });
      }
    })
    .await?;
  watcher.await??;
  assert_eq!(finished.conclusion(), Some(Conclusion::Cancelled));
  assert_orphans_cleaned(&replay, &finished, &["detached"]).await
}

async fn wait_for_file_exists(path: &Path) -> Result<(), String> {
  for _ in 0..1200 {
    if path.exists() {
      return Ok(());
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
  }
  Err(format!("{} never appeared", path.display()))
}

#[tokio::test]
async fn job_without_orphans_logs_header_and_no_termination() -> TestResult {
  let replay = Replay::new().await?;
  let finished = replay.run(script_job(PROBE)?, |_, _, _| {}).await?;
  assert_eq!(finished.conclusion(), Some(Conclusion::Success));
  assert!(finished.has_line(HEADER), "{:?}", finished.lines());
  assert!(!finished.has_line("Terminate orphan process"), "{:?}", finished.lines());
  Ok(())
}

// ---------------------------------------------------------------------------
// AC-3: concurrent jobs and unrelated processes survive another job's sweep
// ---------------------------------------------------------------------------

#[tokio::test]
async fn concurrent_job_and_unrelated_process_survive_cleanup() -> TestResult {
  let mut unrelated = std::process::Command::new("sleep").arg("307").spawn()?;
  let unrelated_pid = unrelated.id();

  let a = Arc::new(Replay::new().await?);
  let b = Arc::new(Replay::new().await?);
  let go = a.pids.join("go");
  let release = b.pids.join("release");
  let job_a = script_job(&detached_script(
    &a.pids_dir(),
    "detached",
    r#"while [ ! -e "$P/go" ]; do sleep 0.05; done"#,
  ))?;
  let job_b = script_job(&detached_script(
    &b.pids_dir(),
    "detached",
    r#"while [ ! -e "$P/release" ]; do sleep 0.05; done"#,
  ))?;

  let run_b = {
    let b = Arc::clone(&b);
    tokio::spawn(async move { b.run(job_b, |_, _, _| {}).await.map_err(|e| e.to_string()) })
  };
  let run_a = {
    let a = Arc::clone(&a);
    tokio::spawn(async move { a.run(job_a, |_, _, _| {}).await.map_err(|e| e.to_string()) })
  };
  // B's detached process must exist before A can finish and sweep.
  wait_for_file(&b.pids.join("detached")).await?;
  std::fs::write(&go, "go")?;
  let finished_a = run_a.await??;
  assert_eq!(finished_a.conclusion(), Some(Conclusion::Success));
  assert_dead(a.pid("detached")?, "job A detached").await?;

  let b_pid = b.pid("detached")?;
  assert_alive(b_pid, "job B detached (after A's sweep)")?;
  assert_alive(unrelated_pid, "unrelated")?;
  assert!(!finished_a.terminated(b_pid));
  assert!(!finished_a.terminated(unrelated_pid));

  std::fs::write(&release, "release")?;
  let finished_b = run_b.await??;
  assert_eq!(finished_b.conclusion(), Some(Conclusion::Success));
  assert_dead(b_pid, "job B detached (after B's sweep)").await?;
  assert_alive(unrelated_pid, "unrelated (after B's sweep)")?;

  unrelated.kill()?;
  unrelated.wait()?;
  Ok(())
}

// ---------------------------------------------------------------------------
// AC-4: opt-outs and the case-insensitive id match
// ---------------------------------------------------------------------------

#[tokio::test]
async fn empty_step_tracking_id_opts_out() -> TestResult {
  let replay = Replay::new().await?;
  let mut job = script_job(&detached_script(&replay.pids_dir(), "detached", "exit 0"))?;
  job
    .steps
    .first_mut()
    .ok_or("step absent")?
    .environment = Some(mapping(&[("RUNNER_TRACKING_ID", "")]));
  let finished = replay.run(job, |_, _, _| {}).await?;
  let pid = replay.pid("detached")?;
  let alive = ps_state(pid)?.is_some_and(|state| !state.starts_with('Z'));
  kill(pid);
  assert_eq!(finished.markers(), [("run".to_owned(), String::new())]);
  assert!(alive, "opted-out process was killed");
  assert!(!finished.terminated(pid));
  Ok(())
}

#[tokio::test]
async fn upper_cased_tracking_id_still_matches() -> TestResult {
  let replay = Replay::new().await?;
  let upper = format!(
    "export RUNNER_TRACKING_ID=\"$(printf %s \"$RUNNER_TRACKING_ID\" | tr a-z A-Z)\"\n{}",
    detached_script(&replay.pids_dir(), "detached", "exit 0")
  );
  let finished = replay.run(script_job(&upper)?, |_, _, _| {}).await?;
  let (_, value) = finished.markers().into_iter().next().ok_or("no marker")?;
  assert!(value.starts_with("GITHUB_"), "{value}");
  let pid = replay.pid("detached")?;
  assert_dead(pid, "upper-cased id").await?;
  assert!(finished.terminated(pid));
  Ok(())
}

#[tokio::test]
async fn process_clean_false_disables_tracking_and_cleanup() -> TestResult {
  let replay = Replay::new().await?;
  let mut job = script_job(&detached_script(&replay.pids_dir(), "detached", "exit 0"))?;
  variable(&mut job, "process.clean", "false");
  let finished = replay.run(job, |_, _, _| {}).await?;
  let pid = replay.pid("detached")?;
  let alive = ps_state(pid)?.is_some_and(|state| !state.starts_with('Z'));
  kill(pid);
  // Upstream sets nothing in this mode, so an outer runner's value (CI) shows.
  let inherited = std::env::var("RUNNER_TRACKING_ID").unwrap_or_else(|_| "unset".to_owned());
  assert_eq!(finished.markers(), [("run".to_owned(), inherited)]);
  assert!(alive, "process.clean=false must not kill");
  assert!(!finished.has_line(HEADER), "{:?}", finished.lines());
  Ok(())
}

// ---------------------------------------------------------------------------
// AC-6: setup failure, failing post, cancellation during cleanup
// ---------------------------------------------------------------------------

#[tokio::test]
async fn failed_job_started_hook_orphan_is_killed() -> TestResult {
  let replay = Replay::new().await?;
  let hook = replay.pids.join("hook.sh");
  std::fs::write(&hook, detached_script(&replay.pids_dir(), "hook", "exit 1"))?;
  let mut job = script_job(PROBE)?;
  variable(
    &mut job,
    "ACTIONS_RUNNER_HOOK_JOB_STARTED",
    &hook.to_string_lossy(),
  );
  let finished = replay.run(job, |_, _, _| {}).await?;
  assert_eq!(finished.conclusion(), Some(Conclusion::Failure));
  let pid = replay.pid("hook")?;
  assert_dead(pid, "job-started hook").await?;
  assert!(finished.terminated(pid), "{:?}", finished.lines());
  Ok(())
}

#[tokio::test]
async fn failing_node_post_orphan_is_killed() -> TestResult {
  let replay = Replay::new().await?;
  let pids = replay.pids_dir();
  let job = script_and_node_job(
    PROBE,
    &[("ORPHAN89_PIDS", &pids), ("ORPHAN89_POST_EXIT", "1")],
  )?;
  replay.install_node_action(&job)?;
  let finished = replay.run(job, |_, _, _| {}).await?;
  assert_eq!(finished.conclusion(), Some(Conclusion::Failure), "{:?}", finished.lines());
  let pid = replay.pid("node-post")?;
  assert_dead(pid, "node post").await?;
  assert!(finished.terminated(pid), "{:?}", finished.lines());
  Ok(())
}

#[tokio::test]
async fn cancellation_during_cleanup_still_kills() -> TestResult {
  let replay = Replay::new().await?;
  let job = script_job(&orphan_script(&replay.pids_dir(), "exit 0"))?;
  let finished = replay
    .run(job, |event, cancel, shutdown| {
      if matches!(event, RunnerEvent::Log { line, .. } if line == HEADER) {
        cancel.cancel();
        shutdown.cancel();
      }
    })
    .await?;
  assert_orphans_cleaned(&replay, &finished, &ORPHANS).await
}
