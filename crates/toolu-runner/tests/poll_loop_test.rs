//! #206: `run` drives a toolu.sh machine (`[provider] kind = "toolu"`) as a
//! `/runner/*` poll loop — S1–S4 and the cancel / revocation / outage /
//! lock / `run --all` guarantees (spec AC-1..AC-7, AC-10, AC-12, AC-13).
//!
//! The real binary runs against `toolu_fixtures::Peer`: toolu.sh's routes
//! over the contract's bodies plus a GitHub broker serving one real shell
//! step per claimed JIT (real RSA keypair). Homes are real temp dirs; the
//! 401s are the live `dev-console.toolu.sh` capture.

use std::path::Path;
use std::time::{Duration, Instant};

use tempfile::TempDir;

use crate::supervisor_fixtures::{
  self as sup, Spawned, TestResult, log_of, reported_conclusion, spawn_logged, spawn_logged_with,
  stop, wait_exit,
};
use crate::toolu_fixtures::{Faults, HOST_TOKEN, Peer, write_machine};

/// Lease the peer grants (a heartbeat must land inside it).
const LEASE_MS: u64 = 3_000;

/// Spawn `run` (log `<home>/run.log`) polling every 100 ms.
fn spawn_run(home: &Path, args: &[&str]) -> TestResult<Spawned> {
  let mut all = vec!["run"];
  all.extend_from_slice(args);
  spawn_logged_with(
    home,
    "run",
    &all,
    &[("TOOLU_RUNNER_POLL_INTERVAL_MS", "100")],
  )
}

/// A claimed JIT config's secrets, encoded and decoded: the envelope, its
/// `.credentials_rsaparams` blob, and that key's private `d`, `p`, `q`.
fn jit_secrets(jit: &str) -> TestResult<Vec<String>> {
  use base64::Engine;
  use base64::engine::general_purpose::STANDARD;
  let envelope: serde_json::Value = serde_json::from_slice(&STANDARD.decode(jit)?)?;
  let blob = envelope
    .get(".credentials_rsaparams")
    .and_then(serde_json::Value::as_str)
    .ok_or("JIT has no .credentials_rsaparams")?;
  let params: serde_json::Value = serde_json::from_slice(&STANDARD.decode(blob)?)?;
  let mut secrets = vec![jit.to_owned(), blob.to_owned()];
  for field in ["d", "p", "q"] {
    let value = params.get(field).and_then(serde_json::Value::as_str);
    secrets.push(value.ok_or(format!("rsaparams has no {field}"))?.to_owned());
  }
  Ok(secrets)
}

/// Every `/runner/*` request carried the machine's host token.
fn assert_bearer(peer: &Peer) {
  let expected = format!("Bearer {HOST_TOKEN}");
  for seen in peer.seen() {
    assert_eq!(
      seen.authorization.as_deref(),
      Some(expected.as_str()),
      "{} {}",
      seen.method,
      seen.path
    );
  }
}

/// The text of every runner log file under `home` (`_diag/runner.log*`,
/// the per-job journals, and the spawned process's own log). An unreadable
/// directory or log is an error, never silently skipped (the callers assert
/// that secrets are absent).
fn all_logs(home: &Path) -> TestResult<String> {
  let mut text = String::new();
  let mut stack = vec![home.to_owned()];
  while let Some(dir) = stack.pop() {
    for entry in std::fs::read_dir(&dir)? {
      let path = entry?.path();
      if path.is_dir() {
        stack.push(path);
      } else if path.extension().is_some_and(|e| e == "log" || e == "jsonl")
        || path
          .file_name()
          .is_some_and(|n| n.to_string_lossy().starts_with("runner.log"))
      {
        text.push_str(&std::fs::read_to_string(&path)?);
      }
    }
  }
  Ok(text)
}

/// S1 / AC-1, AC-10: two queued jobs run back to back; per job the calls
/// are claim → dispatched → heartbeat(s) → complete{done}, each heartbeat
/// inside the lease the previous claim/heartbeat granted; the broker gets
/// both completions; neither the host token nor a JIT reaches a log.
#[tokio::test]
async fn two_queued_jobs_run_back_to_back_with_in_lease_heartbeats() -> TestResult<()> {
  let peer = Peer::start(&["job-a", "job-b"], LEASE_MS, Faults::default(), "sleep 2").await?;
  let home = TempDir::new()?;
  write_machine(home.path(), &peer.server.uri(), HOST_TOKEN, "")?;
  let child = spawn_run(home.path(), &[])?;
  peer.wait_for("/complete", 2).await?;
  stop(child, home.path(), "run").await?;

  for job in ["job-a", "job-b"] {
    let calls: Vec<_> = peer
      .seen()
      .into_iter()
      .filter(|s| s.path.starts_with(&format!("/runner/jobs/{job}/")))
      .collect();
    let verbs: Vec<&str> = calls
      .iter()
      .map(|s| s.path.rsplit('/').next().unwrap_or_default())
      .collect();
    assert_eq!(verbs.first(), Some(&"claim"), "{job}: {verbs:?}");
    assert_eq!(verbs.get(1), Some(&"dispatched"), "{job}: {verbs:?}");
    assert_eq!(verbs.last(), Some(&"complete"), "{job}: {verbs:?}");
    let middle = verbs.get(2..verbs.len() - 1).unwrap_or_default();
    assert!(
      !middle.is_empty() && middle.iter().all(|v| *v == "heartbeat"),
      "{job}: at least one heartbeat between dispatched and complete: {verbs:?}"
    );
    let lease = Duration::from_millis(LEASE_MS);
    let mut granted = calls.first().map(|c| c.at).ok_or("no claim")?;
    for heartbeat in calls.iter().filter(|c| c.path.ends_with("/heartbeat")) {
      assert!(
        heartbeat.at.duration_since(granted) < lease,
        "{job}: heartbeat {:?} after the last lease grant",
        heartbeat.at.duration_since(granted)
      );
      assert_eq!(heartbeat.body, serde_json::json!({ "claimEpoch": 7 }));
      granted = heartbeat.at;
    }
    let complete = calls.last().ok_or("no complete")?;
    assert_eq!(
      complete.body,
      serde_json::json!({ "outcome": "done", "claimEpoch": 7 })
    );
  }
  assert_eq!(sup::count(&peer.server, "POST", "/completejob").await?, 2);
  assert_eq!(reported_conclusion(&peer.server).await?, "succeeded");
  assert_bearer(&peer);

  let logs = all_logs(home.path())?;
  assert!(!logs.is_empty(), "the run left no logs");
  assert!(!logs.contains(HOST_TOKEN), "host token logged");
  for jit in peer.jits() {
    for secret in jit_secrets(&jit)? {
      assert!(!logs.contains(&secret), "a claimed JIT's secret was logged");
    }
  }
  Ok(())
}

/// S2 / AC-2: a lost claim race (409 `taken`) moves on to the next job; no
/// error, the loop keeps running.
#[tokio::test]
async fn a_lost_claim_race_moves_on_to_the_next_job() -> TestResult<()> {
  let faults = Faults {
    taken: vec!["job-a".to_owned()],
    ..Faults::default()
  };
  let peer = Peer::start(&["job-a", "job-b"], 60_000, faults, "true").await?;
  let home = TempDir::new()?;
  write_machine(home.path(), &peer.server.uri(), HOST_TOKEN, "")?;
  let mut child = spawn_run(home.path(), &[])?;
  peer.wait_for("/job-b/complete", 1).await?;
  assert!(
    child.try_wait()?.is_none(),
    "{}",
    log_of(home.path(), "run")
  );
  stop(child, home.path(), "run").await?;

  let claims: Vec<u16> = peer
    .seen_ending("/claim")
    .iter()
    .map(|s| s.status)
    .collect();
  assert_eq!(claims, [409, 200], "{:#?}", peer.paths());
  assert!(peer.seen_ending("/job-a/complete").is_empty());
  let log = log_of(home.path(), "run");
  assert!(
    log.contains("another machine claimed the job first"),
    "{log}"
  );
  assert!(
    !log.contains("ERROR"),
    "a lost race is not an error:\n{log}"
  );
  Ok(())
}

/// S3 / AC-3: the live-captured 401 on the job list after a completed job is
/// fatal: exit 2 naming `toolu-runner setup`.
#[tokio::test]
async fn a_revoked_machine_exits_2_naming_setup() -> TestResult<()> {
  let faults = Faults {
    revoke_after_completes: Some(1),
    ..Faults::default()
  };
  let peer = Peer::start(&["job-a"], 60_000, faults, "true").await?;
  let home = TempDir::new()?;
  write_machine(home.path(), &peer.server.uri(), HOST_TOKEN, "")?;
  let out = wait_exit(spawn_run(home.path(), &[])?).await?;
  let log = log_of(home.path(), "run");
  assert_eq!(out.status.code(), Some(2), "{log}");
  assert!(log.contains("toolu-runner setup"), "{log}");
  assert!(log.contains("invalid or revoked token"), "{log}");
  assert_eq!(peer.seen_ending("/complete").len(), 1);
  Ok(())
}

/// AC-4: a 401 heartbeat mid-job cancels the job (the broker gets a
/// non-success completion), nothing more is sent to toolu.sh, exit 2.
#[tokio::test]
async fn a_401_heartbeat_mid_job_cancels_it_and_exits_2() -> TestResult<()> {
  let faults = Faults {
    heartbeat_401: true,
    ..Faults::default()
  };
  let peer = Peer::start(&["job-a"], LEASE_MS, faults, "sleep 30").await?;
  let home = TempDir::new()?;
  write_machine(home.path(), &peer.server.uri(), HOST_TOKEN, "")?;
  let started = Instant::now();
  let out = wait_exit(spawn_run(home.path(), &[])?).await?;
  let log = log_of(home.path(), "run");
  assert_eq!(out.status.code(), Some(2), "{log}");
  assert!(log.contains("toolu-runner setup"), "{log}");
  assert!(
    started.elapsed() < Duration::from_secs(30),
    "the job was not cancelled"
  );
  assert_ne!(reported_conclusion(&peer.server).await?, "succeeded");
  assert!(
    peer.seen_ending("/complete").is_empty(),
    "{:#?}",
    peer.paths()
  );
  Ok(())
}

/// S4 / AC-5: three 503s on the job list back off (growing gaps, under the
/// 60 s cap), then the job is claimed and completed.
#[tokio::test]
async fn a_503_burst_backs_off_then_recovers() -> TestResult<()> {
  let faults = Faults {
    queued_503: 3,
    ..Faults::default()
  };
  let peer = Peer::start(&["job-a"], 60_000, faults, "true").await?;
  let home = TempDir::new()?;
  write_machine(home.path(), &peer.server.uri(), HOST_TOKEN, "")?;
  let child = spawn_run(home.path(), &[])?;
  peer.wait_for("/job-a/complete", 1).await?;
  stop(child, home.path(), "run").await?;

  let lists = peer.seen_ending("/queued");
  let statuses: Vec<u16> = lists.iter().take(4).map(|s| s.status).collect();
  assert_eq!(statuses, [503, 503, 503, 200]);
  let at: Vec<Instant> = lists.iter().take(4).map(|s| s.at).collect();
  let gaps: Vec<Duration> = at
    .windows(2)
    .filter_map(|w| Some(w.get(1)?.duration_since(*w.first()?)))
    .collect();
  assert_eq!(gaps.len(), 3);
  assert!(
    gaps.windows(2).all(|w| w.first() < w.get(1)),
    "backoff gaps must grow: {gaps:?}"
  );
  assert!(
    gaps.iter().all(|g| *g < Duration::from_secs(60)),
    "{gaps:?}"
  );
  Ok(())
}

/// AC-6: SIGTERM mid-job cancels it gracefully, the broker and toolu.sh both
/// get the report, exit 0.
#[tokio::test]
async fn sigterm_mid_job_cancels_gracefully_and_reports() -> TestResult<()> {
  let peer = Peer::start(&["job-a"], 60_000, Faults::default(), "sleep 30").await?;
  let home = TempDir::new()?;
  write_machine(home.path(), &peer.server.uri(), HOST_TOKEN, "")?;
  let child = spawn_run(home.path(), &[])?;
  sup::wait_for(&peer.server, "POST", "/acquirejob", 1).await?;
  tokio::time::sleep(Duration::from_secs(2)).await;
  let started = Instant::now();
  stop(child, home.path(), "run").await?;
  assert!(
    started.elapsed() < Duration::from_secs(25),
    "not a graceful cancel"
  );
  assert_eq!(sup::count(&peer.server, "POST", "/completejob").await?, 1);
  assert_ne!(reported_conclusion(&peer.server).await?, "succeeded");
  assert_eq!(
    peer.seen_ending("/job-a/complete").len(),
    1,
    "{:#?}",
    peer.paths()
  );
  Ok(())
}

/// AC-7: `run --all` drives a toolu.sh registration's poll loop.
#[tokio::test]
async fn run_all_drives_a_toolu_registration() -> TestResult<()> {
  let peer = Peer::start(&["job-a"], 60_000, Faults::default(), "true").await?;
  let home = TempDir::new()?;
  write_machine(home.path(), &peer.server.uri(), HOST_TOKEN, "")?;
  let child = sup::spawn_run_all(home.path(), false)?;
  peer.wait_for("/job-a/complete", 1).await?;
  stop(child, home.path(), "supervisor").await?;
  assert_bearer(&peer);
  Ok(())
}

/// AC-12: a second `run` on a registration whose `.lock` the first holds
/// exits 2 naming the holder.
#[tokio::test]
async fn a_second_run_on_a_held_registration_exits_2() -> TestResult<()> {
  let peer = Peer::start(&[], 60_000, Faults::default(), "true").await?;
  let home = TempDir::new()?;
  let config = write_machine(home.path(), &peer.server.uri(), HOST_TOKEN, "")?;
  let first = spawn_run(home.path(), &[])?;
  peer.wait_for("/queued", 1).await?;
  let pid = sup::lock_pid(&config).ok_or("no lock holder")?;
  let config_arg = config.to_string_lossy().into_owned();
  let second = spawn_logged(
    home.path(),
    false,
    "second",
    &["run", "--config", &config_arg],
  )?;
  let out = wait_exit(second).await?;
  let log = log_of(home.path(), "second");
  assert_eq!(out.status.code(), Some(2), "{log}");
  assert!(log.contains(&pid.to_string()), "{log}");
  stop(first, home.path(), "run").await
}

/// AC-13: a connection-refused API (a real outage) backs off without
/// exiting; once the config points at the live peer, the loop resumes.
#[tokio::test]
async fn an_outage_backs_off_and_resumes_when_the_peer_returns() -> TestResult<()> {
  let peer = Peer::start(&["job-a"], 60_000, Faults::default(), "true").await?;
  let closed = std::net::TcpListener::bind("127.0.0.1:0")?;
  let dead = format!("http://{}", closed.local_addr()?);
  drop(closed);
  let home = TempDir::new()?;
  let config = write_machine(home.path(), &dead, HOST_TOKEN, "")?;
  let mut child = spawn_run(home.path(), &[])?;
  sup::wait_until("two failed lists", || {
    log_of(home.path(), "run")
      .matches("listing toolu.sh jobs failed; backing off")
      .count()
      >= 2
  })
  .await?;
  assert!(
    child.try_wait()?.is_none(),
    "{}",
    log_of(home.path(), "run")
  );
  std::fs::write(
    &config,
    crate::toolu_fixtures::machine_toml(home.path(), &peer.server.uri(), ""),
  )?;
  peer.wait_for("/job-a/complete", 1).await?;
  stop(child, home.path(), "run").await
}

/// Failure modes refused at start with exit 2: an ephemeral machine, and an
/// empty host token (naming `toolu-runner setup`); neither polls.
#[tokio::test]
async fn an_ephemeral_machine_or_an_empty_token_is_refused_at_start() -> TestResult<()> {
  let peer = Peer::start(&["job-a"], 60_000, Faults::default(), "true").await?;
  let home = TempDir::new()?;
  write_machine(
    home.path(),
    &peer.server.uri(),
    HOST_TOKEN,
    "ephemeral = true",
  )?;
  let out = wait_exit(spawn_run(home.path(), &[])?).await?;
  let log = log_of(home.path(), "run");
  assert_eq!(out.status.code(), Some(2), "{log}");
  assert!(log.contains("cannot be ephemeral"), "{log}");

  write_machine(home.path(), &peer.server.uri(), "", "")?;
  let out = wait_exit(spawn_run(home.path(), &[])?).await?;
  let log = log_of(home.path(), "run");
  assert_eq!(out.status.code(), Some(2), "{log}");
  assert!(log.contains("toolu-runner setup"), "{log}");
  assert!(peer.seen().is_empty(), "{:#?}", peer.paths());
  Ok(())
}

/// `slots-full` is a wait, not a failure: the loop idles at the poll
/// interval (never backs off), then claims once a slot frees.
#[tokio::test]
async fn slots_full_waits_then_claims() -> TestResult<()> {
  let faults = Faults {
    claim_refusals: vec![
      (409, "claim refused: slots-full"),
      (409, "claim refused: slots-full"),
    ],
    ..Faults::default()
  };
  let peer = Peer::start(&["job-a"], 60_000, faults, "true").await?;
  let home = TempDir::new()?;
  write_machine(home.path(), &peer.server.uri(), HOST_TOKEN, "")?;
  let child = spawn_run(home.path(), &[])?;
  peer.wait_for("/job-a/complete", 1).await?;
  stop(child, home.path(), "run").await?;

  let claims: Vec<u16> = peer
    .seen_ending("/claim")
    .iter()
    .map(|s| s.status)
    .collect();
  assert_eq!(claims, [409, 409, 200], "{:#?}", peer.paths());
  let log = log_of(home.path(), "run");
  assert!(log.contains("slots are full; waiting"), "{log}");
  assert!(
    !log.contains("backing off"),
    "slots-full must not back off:\n{log}"
  );
  Ok(())
}

/// The claim's deadline cancels a job still running at it; the cancelled
/// job is reported `done` (it ran to a conclusion) and the broker hears a
/// non-success conclusion.
#[tokio::test]
async fn the_claim_deadline_cancels_a_running_job() -> TestResult<()> {
  let faults = Faults {
    deadline_ms: Some(3_000),
    ..Faults::default()
  };
  let peer = Peer::start(&["job-a"], 60_000, faults, "sleep 30").await?;
  let home = TempDir::new()?;
  write_machine(home.path(), &peer.server.uri(), HOST_TOKEN, "")?;
  let started = Instant::now();
  let child = spawn_run(home.path(), &[])?;
  peer.wait_for("/job-a/complete", 1).await?;
  assert!(
    started.elapsed() < Duration::from_secs(25),
    "the deadline did not cancel the job"
  );
  stop(child, home.path(), "run").await?;

  assert_ne!(reported_conclusion(&peer.server).await?, "succeeded");
  let complete = peer.seen_ending("/job-a/complete");
  assert_eq!(
    complete.first().map(|c| c.body.clone()),
    Some(serde_json::json!({ "outcome": "done", "claimEpoch": 7 }))
  );
  let log = log_of(home.path(), "run");
  assert!(log.contains("the claim's deadline passed"), "{log}");
  Ok(())
}

/// A claim whose deadline already passed is never started: no broker
/// session, and toolu.sh gets `failed` with the reason.
#[tokio::test]
async fn a_claim_past_its_deadline_is_reported_failed_without_running() -> TestResult<()> {
  let faults = Faults {
    deadline_ms: Some(-1_000),
    ..Faults::default()
  };
  let peer = Peer::start(&["job-a"], 60_000, faults, "true").await?;
  let home = TempDir::new()?;
  write_machine(home.path(), &peer.server.uri(), HOST_TOKEN, "")?;
  let child = spawn_run(home.path(), &[])?;
  peer.wait_for("/job-a/complete", 1).await?;
  stop(child, home.path(), "run").await?;

  assert_eq!(sup::count(&peer.server, "POST", "/acquirejob").await?, 0);
  assert!(peer.seen_ending("/job-a/dispatched").is_empty());
  let complete = peer.seen_ending("/job-a/complete");
  assert_eq!(
    complete.first().map(|c| c.body.clone()),
    Some(serde_json::json!({
      "outcome": "failed",
      "errorMessage": "config error: the claim's deadline passed before the job started",
      "claimEpoch": 7,
    })),
    "{:#?}",
    peer.paths()
  );
  Ok(())
}

/// A 409 heartbeat (the claim is no longer this machine's) stops renewing;
/// the job still runs to its end and is reported.
#[tokio::test]
async fn a_lost_claim_stops_renewing_and_the_job_finishes() -> TestResult<()> {
  let faults = Faults {
    heartbeat_409: true,
    ..Faults::default()
  };
  let peer = Peer::start(&["job-a"], LEASE_MS, faults, "sleep 4").await?;
  let home = TempDir::new()?;
  write_machine(home.path(), &peer.server.uri(), HOST_TOKEN, "")?;
  let child = spawn_run(home.path(), &[])?;
  peer.wait_for("/job-a/complete", 1).await?;
  stop(child, home.path(), "run").await?;

  assert_eq!(
    peer.seen_ending("/heartbeat").len(),
    1,
    "{:#?}",
    peer.paths()
  );
  assert_eq!(reported_conclusion(&peer.server).await?, "succeeded");
  let log = log_of(home.path(), "run");
  assert!(
    log.contains("the claim was lost; the job runs to its end"),
    "{log}"
  );
  Ok(())
}

/// A transient `complete` failure is retried until toolu.sh takes it.
#[tokio::test]
async fn a_transient_complete_failure_is_retried() -> TestResult<()> {
  let faults = Faults {
    complete_503: 2,
    ..Faults::default()
  };
  let peer = Peer::start(&["job-a"], 60_000, faults, "true").await?;
  let home = TempDir::new()?;
  write_machine(home.path(), &peer.server.uri(), HOST_TOKEN, "")?;
  let child = spawn_run(home.path(), &[])?;
  peer.wait_for("/job-a/complete", 3).await?;
  stop(child, home.path(), "run").await?;

  let statuses: Vec<u16> = peer
    .seen_ending("/job-a/complete")
    .iter()
    .map(|s| s.status)
    .collect();
  assert_eq!(statuses, [503, 503, 200]);
  Ok(())
}

/// `run --once` runs one claimed job, reports it and exits 0, leaving the
/// rest of the queue.
#[tokio::test]
async fn once_runs_one_job_and_exits_0() -> TestResult<()> {
  let peer = Peer::start(&["job-a", "job-b"], 60_000, Faults::default(), "true").await?;
  let home = TempDir::new()?;
  write_machine(home.path(), &peer.server.uri(), HOST_TOKEN, "")?;
  let out = wait_exit(spawn_run(home.path(), &["--once"])?).await?;
  let log = log_of(home.path(), "run");
  assert_eq!(out.status.code(), Some(0), "{log}");
  assert_eq!(
    peer.seen_ending("/complete").len(),
    1,
    "{:#?}",
    peer.paths()
  );
  assert!(peer.seen_ending("/job-b/claim").is_empty());
  Ok(())
}

/// The other claim refusals: `cap-reached` waits, a 404 (job gone) and a
/// reply outside the contract (400) try the next job, a 503 backs off; the
/// queue still drains.
#[tokio::test]
async fn claim_refusals_wait_skip_or_back_off_by_kind() -> TestResult<()> {
  let faults = Faults {
    claim_refusals: vec![
      (409, "claim refused: cap-reached"),
      (404, "claim refused: not-found"),
      (400, "bad request"),
      (503, "unavailable"),
    ],
    ..Faults::default()
  };
  let peer = Peer::start(&["job-a", "job-b"], 60_000, faults, "true").await?;
  let home = TempDir::new()?;
  write_machine(home.path(), &peer.server.uri(), HOST_TOKEN, "")?;
  let child = spawn_run(home.path(), &[])?;
  peer.wait_for("/complete", 2).await?;
  stop(child, home.path(), "run").await?;

  let claims: Vec<(String, u16)> = peer
    .seen_ending("/claim")
    .iter()
    .map(|s| (s.path.clone(), s.status))
    .collect();
  let expected = [
    ("/runner/jobs/job-a/claim", 409), // cap-reached: wait, re-list
    ("/runner/jobs/job-a/claim", 404), // gone: try the next job
    ("/runner/jobs/job-b/claim", 400), // off-contract: try the next job
    ("/runner/jobs/job-a/claim", 503), // transient: back off, re-list
    ("/runner/jobs/job-a/claim", 200),
  ];
  for (i, (path, status)) in expected.iter().enumerate() {
    assert_eq!(
      claims.get(i).map(|(p, s)| (p.as_str(), *s)),
      Some((*path, *status)),
      "{claims:#?}"
    );
  }
  let log = log_of(home.path(), "run");
  for line in [
    "toolu.sh refused the claim (cap-reached); waiting",
    "the job is no longer claimable; trying the next",
    "toolu.sh answered the claim unexpectedly",
    "claiming the job failed",
  ] {
    assert!(log.contains(line), "{line}:\n{log}");
  }
  Ok(())
}

/// A 401 on a claim, on `dispatched` (mid-job) or on `complete` is the same
/// revocation: exit 2 naming `toolu-runner setup`, nothing sent after it.
#[tokio::test]
async fn a_401_on_claim_dispatched_or_complete_is_fatal() -> TestResult<()> {
  for verb in ["claim", "dispatched", "complete"] {
    let faults = Faults {
      revoke_verb: Some(verb),
      ..Faults::default()
    };
    let peer = Peer::start(&["job-a"], 60_000, faults, "true").await?;
    let home = TempDir::new()?;
    write_machine(home.path(), &peer.server.uri(), HOST_TOKEN, "")?;
    let out = wait_exit(spawn_run(home.path(), &[])?).await?;
    let log = log_of(home.path(), "run");
    assert_eq!(out.status.code(), Some(2), "{verb}: {log}");
    assert!(log.contains("toolu-runner setup"), "{verb}: {log}");
    let last = peer.seen().last().map(|s| (s.path.clone(), s.status));
    assert_eq!(
      last,
      Some((format!("/runner/jobs/job-a/{verb}"), 401)),
      "{verb}: nothing may follow the 401: {:#?}",
      peer.paths()
    );
  }
  Ok(())
}

/// `.pending_remove` (a `remove` of a running registration) stops the loop
/// between jobs with exit 0.
#[tokio::test]
async fn pending_remove_stops_the_loop_between_jobs() -> TestResult<()> {
  let peer = Peer::start(&[], 60_000, Faults::default(), "true").await?;
  let home = TempDir::new()?;
  let config = write_machine(home.path(), &peer.server.uri(), HOST_TOKEN, "")?;
  let child = spawn_run(home.path(), &[])?;
  peer.wait_for("/queued", 1).await?;
  std::fs::write(config.with_file_name(".pending_remove"), "")?;
  let out = wait_exit(child).await?;
  let log = log_of(home.path(), "run");
  assert_eq!(out.status.code(), Some(0), "{log}");
  assert!(
    log.contains("stopping the toolu.sh poll loop between jobs"),
    "{log}"
  );
  Ok(())
}

/// A re-paired host token (`credentials.json` rewritten) is used from the
/// next pass on, and masked like the first.
#[tokio::test]
async fn a_rotated_host_token_is_used_on_the_next_pass() -> TestResult<()> {
  /// A second synthetic token of the contract's shape.
  const ROTATED: &str = "tlu_h_rotated206rotated206rotated206rotated206rot";
  let peer = Peer::start(&[], 60_000, Faults::default(), "true").await?;
  let home = TempDir::new()?;
  let config = write_machine(home.path(), &peer.server.uri(), HOST_TOKEN, "")?;
  let child = spawn_run(home.path(), &[])?;
  peer.wait_for("/queued", 1).await?;
  std::fs::write(
    config.with_file_name("credentials.json"),
    serde_json::json!({ "access_token": ROTATED, "issued_at": "2026-10-24T00:00:00Z" }).to_string(),
  )?;
  let rotated = format!("Bearer {ROTATED}");
  sup::wait_until("a list with the rotated token", || {
    peer
      .seen()
      .iter()
      .any(|s| s.authorization.as_deref() == Some(rotated.as_str()))
  })
  .await?;
  stop(child, home.path(), "run").await?;
  let logs = all_logs(home.path())?;
  assert!(!logs.contains(HOST_TOKEN), "original host token logged");
  assert!(!logs.contains(ROTATED), "rotated host token logged");
  Ok(())
}
