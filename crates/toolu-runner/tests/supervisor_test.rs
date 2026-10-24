//! `run --all` (#208) through the real binary: one process drives every
//! registration under a temporary runner home against loopback GitHub peers
//! (wiremock brokers + real RSA JIT envelopes, real job steps).
//!
//! - AC-2: two registrations poll concurrently, each under its own `.lock`.
//! - AC-3: a registration added + SIGHUP starts polling; the others keep
//!   their sessions and an in-flight job finishes `succeeded`.
//! - AC-6: SIGTERM ends every session and exits 0.
//! - AC-8: a registration a foreground `run` holds is never driven twice.
//! - AC-10: an org and a repo registration run side by side.
//! - AC-11: an ephemeral registration runs one job and removes itself.
//! - AC-16: a second `run --all` on the same home exits 2.

use std::path::Path;
use std::time::Duration;

use crate::supervisor_fixtures::{
  Scope, TestResult, count, idle_broker, job_broker, lock_is_held, lock_pid, log_of,
  reported_conclusion, runner, signal, spawn_logged, spawn_run_all, stop, wait_exit, wait_for,
  wait_until, write_registration,
};

/// AC-2 + AC-6: both registrations get a session and long-poll from one
/// process, each `.lock` names that process and is held; SIGTERM deletes
/// both sessions and exits 0.
#[tokio::test]
async fn two_registrations_poll_concurrently_each_under_its_own_lock() -> TestResult<()> {
  let home = tempfile::tempdir()?;
  let (a, b) = (idle_broker().await?, idle_broker().await?);
  let config_a = write_registration(home.path(), &a, Scope::Repo("octo", "a"), false)?;
  let config_b = write_registration(home.path(), &b, Scope::Repo("octo", "b"), false)?;

  let child = spawn_run_all(home.path(), false)?;
  wait_for(&a, "GET", "/message", 2).await?;
  wait_for(&b, "GET", "/message", 2).await?;
  for config in [&config_a, &config_b] {
    assert_eq!(
      lock_pid(config),
      Some(child.id()),
      "{}",
      log_of(home.path(), "supervisor")
    );
    assert!(lock_is_held(config)?, "{} must be held", config.display());
  }
  assert_eq!(count(&a, "POST", "/session").await?, 1);
  assert_eq!(count(&b, "POST", "/session").await?, 1);

  stop(child, home.path(), "supervisor").await?;
  assert_eq!(count(&a, "DELETE", "/session/").await?, 1);
  assert_eq!(count(&b, "DELETE", "/session/").await?, 1);
  Ok(())
}

/// AC-3: a registration written while the supervisor runs starts on
/// SIGHUP; the idle ones keep their single session and the registration
/// running a real `sleep 3` job finishes it `succeeded` with one acquire —
/// all in the same process.
#[tokio::test]
async fn sighup_starts_a_new_registration_without_disturbing_the_others() -> TestResult<()> {
  let home = tempfile::tempdir()?;
  let (a, b) = (idle_broker().await?, idle_broker().await?);
  let busy = job_broker("sleep 3").await?;
  write_registration(home.path(), &a, Scope::Repo("octo", "a"), false)?;
  write_registration(home.path(), &b, Scope::Repo("octo", "b"), false)?;
  write_registration(home.path(), &busy, Scope::Repo("octo", "busy"), false)?;

  let child = spawn_run_all(home.path(), true)?;
  let pid = child.id();
  wait_for(&a, "GET", "/message", 1).await?;
  wait_for(&b, "GET", "/message", 1).await?;
  wait_for(&busy, "POST", "/acquirejob", 1).await?;

  let added = idle_broker().await?;
  let config_c = write_registration(home.path(), &added, Scope::Repo("octo", "c"), false)?;
  signal(&child, "-HUP")?;
  wait_for(&added, "POST", "/session", 1).await?;
  wait_for(&added, "GET", "/message", 1).await?;
  assert_eq!(lock_pid(&config_c), Some(pid));

  wait_for(&busy, "POST", "/completejob", 1).await?;
  assert_eq!(reported_conclusion(&busy).await?, "succeeded");
  assert_eq!(count(&busy, "POST", "/acquirejob").await?, 1);
  for idle in [&a, &b] {
    assert_eq!(
      count(idle, "POST", "/session").await?,
      1,
      "kept its session"
    );
    assert_eq!(count(idle, "DELETE", "/session/").await?, 0);
  }
  let mut child = child;
  assert!(
    child.try_wait()?.is_none(),
    "no restart: the same process runs on"
  );
  stop(child, home.path(), "supervisor").await
}

/// AC-3 (`register`): with no supervisor, `register` says nothing about a
/// service; while one runs, `register` signals it and the new registration
/// starts polling with no manual SIGHUP, the running one undisturbed.
#[tokio::test]
async fn register_starts_its_loop_without_a_manual_signal() -> TestResult<()> {
  let home = tempfile::tempdir()?;
  let first = idle_broker().await?;
  let register = |peer: &str, repo: &str| {
    runner(home.path(), true)
      .args(["register", "--url", &format!("{peer}/octo/{repo}")])
      .args(["--name", &format!("toolu-208-{repo}")])
      .output()
  };
  let alone = register(&first.uri(), "first")?;
  let alone_out = String::from_utf8_lossy(&alone.stdout);
  assert_eq!(
    alone.status.code(),
    Some(0),
    "{alone_out}{}",
    String::from_utf8_lossy(&alone.stderr)
  );
  assert!(!alone_out.contains("picks it up"), "{alone_out}");

  let child = spawn_run_all(home.path(), false)?;
  wait_for(&first, "GET", "/message", 1).await?;
  let added = idle_broker().await?;
  let out = register(&added.uri(), "added")?;
  let stdout = String::from_utf8_lossy(&out.stdout);
  assert_eq!(
    out.status.code(),
    Some(0),
    "{stdout}{}",
    String::from_utf8_lossy(&out.stderr)
  );
  assert!(
    stdout.contains(&format!(
      "the toolu-runner service (pid {}) picks it up now",
      child.id()
    )),
    "{stdout}"
  );
  wait_for(&added, "POST", "/session", 1).await?;
  wait_for(&added, "GET", "/message", 1).await?;
  // A `--config` outside the runner home is not the supervisor's to run:
  // no pickup is claimed.
  let elsewhere = tempfile::tempdir()?;
  let outside = runner(home.path(), true)
    .args([
      "register",
      "--url",
      &format!("{}/octo/outside", added.uri()),
    ])
    .arg("--config")
    .arg(elsewhere.path().join("config.toml"))
    .output()?;
  assert_eq!(
    outside.status.code(),
    Some(0),
    "{}",
    String::from_utf8_lossy(&outside.stderr)
  );
  assert!(!String::from_utf8_lossy(&outside.stdout).contains("picks it up"));
  // An organization URL (#201's org-scoped mint) lands in `orgs/<org>/` and
  // is picked up the same way.
  let org = idle_broker().await?;
  let org_out = runner(home.path(), true)
    .args(["register", "--url", &format!("{}/acme", org.uri())])
    .args(["--name", "toolu-208-acme"])
    .output()?;
  let org_stdout = String::from_utf8_lossy(&org_out.stdout);
  assert_eq!(
    org_out.status.code(),
    Some(0),
    "{org_stdout}{}",
    String::from_utf8_lossy(&org_out.stderr)
  );
  assert!(org_stdout.contains("picks it up now"), "{org_stdout}");
  assert_eq!(
    count(
      &org,
      "POST",
      "/api/v3/orgs/acme/actions/runners/generate-jitconfig"
    )
    .await?,
    1
  );
  wait_for(&org, "POST", "/session", 1).await?;
  assert_eq!(
    lock_pid(&home.path().join("orgs/acme/config.toml")),
    Some(child.id())
  );
  let config = home.path().join("runners/octo/added/config.toml");
  assert_eq!(lock_pid(&config), Some(child.id()));
  assert_eq!(count(&first, "POST", "/session").await?, 1);
  stop(child, home.path(), "supervisor").await
}

/// AC-8: while a foreground `run --config A` holds A's lock the supervisor
/// drives only B; once that process exits, the supervisor takes A.
#[tokio::test]
async fn a_registration_held_by_a_foreground_run_is_never_driven_twice() -> TestResult<()> {
  let home = tempfile::tempdir()?;
  let (a, b) = (idle_broker().await?, idle_broker().await?);
  let config_a = write_registration(home.path(), &a, Scope::Repo("octo", "a"), false)?;
  write_registration(home.path(), &b, Scope::Repo("octo", "b"), false)?;
  let config_arg = config_a.to_string_lossy().into_owned();

  let foreground = spawn_logged(
    home.path(),
    false,
    "foreground",
    &["run", "--config", &config_arg],
  )?;
  let foreground_pid = foreground.id();
  wait_for(&a, "GET", "/message", 1).await?;
  assert_eq!(lock_pid(&config_a), Some(foreground_pid));

  let supervisor = spawn_run_all(home.path(), false)?;
  let supervisor_pid = supervisor.id();
  wait_for(&b, "GET", "/message", 1).await?;
  tokio::time::sleep(Duration::from_secs(3)).await;
  assert_eq!(
    lock_pid(&config_a),
    Some(foreground_pid),
    "never taken while held"
  );
  assert_eq!(count(&a, "POST", "/session").await?, 1);

  stop(foreground, home.path(), "foreground").await?;
  wait_until("the supervisor to take A", || {
    lock_pid(&config_a) == Some(supervisor_pid)
  })
  .await?;
  wait_for(&a, "POST", "/session", 2).await?;
  assert!(
    log_of(home.path(), "supervisor").contains("held by another process"),
    "{}",
    log_of(home.path(), "supervisor")
  );
  stop(supervisor, home.path(), "supervisor").await
}

/// AC-10: an organization registration (`orgs/acme/`) and a repository
/// registration of the same owner run side by side in one process.
#[tokio::test]
async fn an_org_and_a_repo_registration_run_side_by_side() -> TestResult<()> {
  let home = tempfile::tempdir()?;
  let (org, repo) = (idle_broker().await?, idle_broker().await?);
  let config_org = write_registration(home.path(), &org, Scope::Org("acme"), false)?;
  let config_repo = write_registration(home.path(), &repo, Scope::Repo("acme", "web"), false)?;
  assert!(config_org.starts_with(home.path().join("orgs").join("acme")));

  let child = spawn_run_all(home.path(), false)?;
  wait_for(&org, "GET", "/message", 1).await?;
  wait_for(&repo, "GET", "/message", 1).await?;
  assert_eq!(lock_pid(&config_org), Some(child.id()));
  assert_eq!(lock_pid(&config_repo), Some(child.id()));
  stop(child, home.path(), "supervisor").await
}

/// AC-11: an ephemeral registration takes one job, unregisters and deletes
/// itself; the idle registration keeps polling and the process stays up.
#[tokio::test]
async fn an_ephemeral_registration_runs_one_job_and_removes_itself() -> TestResult<()> {
  let home = tempfile::tempdir()?;
  let ephemeral = job_broker("true").await?;
  let idle = idle_broker().await?;
  let config_e = write_registration(home.path(), &ephemeral, Scope::Repo("octo", "eph"), true)?;
  write_registration(home.path(), &idle, Scope::Repo("octo", "idle"), false)?;

  let mut child = spawn_run_all(home.path(), true)?;
  wait_until("the ephemeral registration to delete itself", || {
    !config_e.exists()
  })
  .await?;
  assert_eq!(count(&ephemeral, "POST", "/acquirejob").await?, 1);
  assert_eq!(reported_conclusion(&ephemeral).await?, "succeeded");
  let delete = format!(
    "/api/v3/repos/octo/eph/actions/runners/{}",
    crate::supervisor_fixtures::RUNNER_ID
  );
  assert_eq!(count(&ephemeral, "DELETE", &delete).await?, 1);
  assert_eq!(
    count(
      &ephemeral,
      "POST",
      "/api/v3/repos/octo/eph/actions/runners/generate-jitconfig"
    )
    .await?,
    0
  );

  let before = count(&idle, "GET", "/message").await?;
  wait_for(&idle, "GET", "/message", before + 2).await?;
  assert!(child.try_wait()?.is_none(), "the service keeps running");
  stop(child, home.path(), "supervisor").await
}

/// AC-16: a second `run --all` on a supervised home exits 2 naming the
/// first supervisor's pid, and opens no session.
#[tokio::test]
async fn a_second_supervisor_on_the_same_home_exits_2() -> TestResult<()> {
  let home = tempfile::tempdir()?;
  let a = idle_broker().await?;
  write_registration(home.path(), &a, Scope::Repo("octo", "a"), false)?;
  let first = spawn_run_all(home.path(), false)?;
  wait_for(&a, "GET", "/message", 1).await?;

  let second = spawn_logged(home.path(), false, "second", &["run", "--all"])?;
  let out = wait_exit(second).await?;
  let log = log_of(home.path(), "second");
  assert_eq!(out.status.code(), Some(2), "{log}");
  assert!(log.contains("already supervises"), "{log}");
  assert!(log.contains(&first.id().to_string()), "{log}");
  assert_eq!(count(&a, "POST", "/session").await?, 1);
  stop(first, home.path(), "supervisor").await
}

/// `run --all` refuses `--config` and `--once` (one registration vs all).
#[test]
fn run_all_conflicts_with_config_and_once() -> TestResult<()> {
  let home = tempfile::tempdir()?;
  for extra in [["--config", "x.toml"].as_slice(), ["--once"].as_slice()] {
    let out = runner(home.path(), false)
      .args(["run", "--all"])
      .args(extra)
      .output()?;
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("cannot be used with"));
  }
  assert!(!Path::new(&home.path().join("supervisor.lock")).exists());
  Ok(())
}
