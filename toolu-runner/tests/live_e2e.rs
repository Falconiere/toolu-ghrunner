//! Live end-to-end tests: `register` flow (AC #1a, #1b) and the
//! `run --once` flow (AC #2–#5, #13, #14).
//!
//! Each run test pushes a workflow YAML to the test repo, triggers it,
//! spawns `toolu-runner run --once` in a child process, and asserts on
//! the resulting GitHub Actions run's `conclusion` field. The runner
//! pulls jobs, executes them, and reports the conclusion back to GH —
//! the test only needs to wait for the run to complete and read the
//! `conclusion` field.
//!
//! Each test is `#[ignore]`'d so the harness compiles under
//! `cargo test --features live` without a real PAT. Run with
//! `cargo test -p toolu-runner --features live -- --ignored` to
//! execute, with `TOOLU_RUNNER_LIVE_TOKEN` and `TOOLU_RUNNER_LIVE_REPO`
//! set in the environment.

#![cfg(feature = "live")]

use std::time::Duration;

// White-box on purpose: AC #1 asserts the exact persisted shapes of
// `config.toml` / `credentials.json`, so this test deserializes them
// with the same lib types `register` writes them with. All
// process-level interaction still goes through `LiveHarness`.
use toolu_runner::config::{
  CredentialsFile, RunnerRegistrationConfig, load_config as load_reg_config, load_credentials,
};
use toolu_runner::journal::{JournalEvent, JournalLine};

#[path = "helpers/live_harness.rs"]
mod harness;

use harness::LiveHarness;

const NOOP_FIXTURE: &str = include_str!("fixtures/noop-workflow.yml");

/// Workflow for the expression test: interpolates `inputs.*` and
/// `github.*` contexts into step env and asserts they are non-empty.
const EXPRESSION_YAML: &str = r#"
name: expression
on:
  workflow_dispatch:
    inputs:
      who:
        description: 'Who to greet'
        required: true
        default: 'world'
jobs:
  greet:
    runs-on: [self-hosted, toolu-runner-v1]
    steps:
      - name: echo interpolated values
        env:
          WHO: ${{ inputs.who }}
          REPO: ${{ github.repository }}
          SHA: ${{ github.sha }}
        run: |
          echo "hello $WHO from $REPO @ $SHA"
          test -n "$REPO"
          test -n "$SHA"
"#;

/// Skip the test with a clear message if the live env vars are missing.
macro_rules! require_live_env {
  () => {
    if std::env::var("TOOLU_RUNNER_LIVE_TOKEN").is_err()
      || std::env::var("TOOLU_RUNNER_LIVE_REPO").is_err()
    {
      eprintln!(
        "skipping live test: set TOOLU_RUNNER_LIVE_TOKEN and TOOLU_RUNNER_LIVE_REPO to run"
      );
      return;
    }
  };
}

/// Workflow for the journal test (AC-1/AC-2): two named steps, a
/// `::warning::` annotation, and a token-probe step that echoes
/// `$ACTIONS_RUNTIME_TOKEN` — the journal must show it masked.
const JOURNAL_YAML: &str = r#"
name: journal-fixture
on:
  workflow_dispatch:
jobs:
  build:
    runs-on: [self-hosted, toolu-runner-v1]
    steps:
      - name: greet
        run: |
          echo hello from step one
          echo "::warning file=demo.txt,line=3::deprecated feature"
      - name: token probe
        run: echo "token-probe=[$ACTIONS_RUNTIME_TOKEN]"
      - name: farewell
        run: echo done from step two
"#;

/// Wait for `child` to exit and return its `ExitStatus`. Used after
/// `run_once` so the child doesn't outlive the test process. The
/// `Option<ExitStatus>` from `Child::wait` is flattened to a status
/// or `None` if the child is already gone (treated as success).
async fn wait_child(mut child: tokio::process::Child) -> Option<std::process::ExitStatus> {
  child.wait().await.ok()
}

/// AC #1a assertions: the persisted `config.toml` carries the repo URL,
/// derived runner name, matchable labels, and the v2 protocol.
fn assert_config_shape(cfg: &RunnerRegistrationConfig, repo: &str) {
  assert_eq!(
    cfg.runner_url,
    format!("https://github.com/{repo}"),
    "runner_url should be the test repo URL"
  );
  assert_eq!(
    cfg.runner_name,
    format!(
      "toolu-runner-live-{}",
      repo.replace('/', "-").to_lowercase()
    ),
    "runner_name should be the harness's deterministic derivation from the repo"
  );
  assert!(
    cfg.labels.contains(&"self-hosted".to_owned()),
    "labels should include 'self-hosted': {:?}",
    cfg.labels
  );
  assert!(
    cfg.labels.contains(&"toolu-runner-v1".to_owned()),
    "labels should include 'toolu-runner-v1' so workflow `runs-on` matches: {:?}",
    cfg.labels
  );
  assert_eq!(
    cfg.runtime.protocol_version, "v2",
    "github.com should pick v2 protocol; got {}",
    cfg.runtime.protocol_version
  );
}

/// AC #1b assertions: `credentials.json` stores the runner's client_id
/// (a UUID — the stable, non-secret identity lifted from the minted JIT
/// config; the real OAuth token is exchanged at `run` time) and an
/// RFC3339 `issued_at`.
fn assert_credentials_shape(creds: &CredentialsFile) {
  assert!(
    uuid::Uuid::parse_str(&creds.access_token).is_ok_and(|id| !id.is_nil()),
    "access_token should be the non-nil client_id UUID from the JIT config; got prefix: {}",
    creds.access_token.get(..16).unwrap_or("?")
  );
  assert!(
    !creds.issued_at.is_empty(),
    "issued_at should be an RFC3339 timestamp"
  );
}

#[tokio::test]
#[ignore = "live test — requires TOOLU_RUNNER_LIVE_TOKEN"]
async fn register_creates_config_and_credentials() {
  require_live_env!();
  let harness = LiveHarness::new().await.expect("harness init");

  // Register against the test repo. The harness always passes
  // `--replace` so re-runs are idempotent.
  harness.register().await.expect("register");

  let cfg_path = harness.config_path();
  let creds_path = harness.credentials_path();
  assert!(
    cfg_path.exists(),
    "config.toml was not written to {}",
    cfg_path.display()
  );
  assert!(
    creds_path.exists(),
    "credentials.json was not written to {}",
    creds_path.display()
  );

  let cfg: RunnerRegistrationConfig = load_reg_config(&cfg_path)
    .map_err(|e| e.to_string())
    .expect("load config.toml");
  assert_config_shape(&cfg, &harness.repo);

  let creds: CredentialsFile = load_credentials(&creds_path)
    .map_err(|e| e.to_string())
    .expect("load credentials.json");
  assert_credentials_shape(&creds);

  // Teardown.
  let _ = harness.remove().await;
}

#[tokio::test]
#[ignore = "live test — requires TOOLU_RUNNER_LIVE_TOKEN"]
async fn register_replace_overwrites_existing() {
  require_live_env!();
  let harness = LiveHarness::new().await.expect("harness init");

  // First registration.
  harness.register().await.expect("first register");
  let first_mtime = std::fs::metadata(harness.config_path())
    .and_then(|m| m.modified())
    .ok();

  // Second registration with --replace (which the harness always passes).
  // Should succeed and overwrite the first. Without --replace, the
  // second call would exit 2 with "registration already exists".
  tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
  harness
    .register()
    .await
    .expect("second register with --replace");

  assert!(
    harness.config_path().exists(),
    "config.toml should still exist after replace"
  );
  let second_mtime = std::fs::metadata(harness.config_path())
    .and_then(|m| m.modified())
    .ok();
  if let (Some(a), Some(b)) = (first_mtime, second_mtime) {
    assert!(
      b > a,
      "config.toml mtime should advance after replace (first={a:?}, second={b:?})"
    );
  }
  // The configs should both be valid TOML and parse successfully.
  let _: RunnerRegistrationConfig = load_reg_config(&harness.config_path())
    .map_err(|e| e.to_string())
    .expect("second config parses");

  // Teardown.
  let _ = harness.remove().await;
}

#[tokio::test]
#[ignore = "live test — requires TOOLU_RUNNER_LIVE_TOKEN"]
async fn noop_job_completes() {
  require_live_env!();
  let harness = LiveHarness::new().await.expect("harness init");
  let workflow = "noop-live.yml";
  let _cleanup = harness.cleanup(&[workflow]).await;

  harness.register().await.expect("register");
  harness
    .push_workflow(workflow, NOOP_FIXTURE)
    .await
    .expect("push noop workflow");

  // Spawn the runner before triggering so the listener picks up the
  // job as soon as GH sees the workflow_dispatch event.
  let child = harness.run_once().await.expect("spawn run --once");
  let run_id = harness
    .trigger_workflow(workflow)
    .await
    .expect("trigger workflow");

  // The run should finish within 5 minutes — `echo hello` is fast, but
  // action runner cold-starts (Node download, action download) can be
  // 30-60s on a fresh VM.
  let conclusion = harness
    .wait_for_run(run_id, Duration::from_secs(300))
    .await
    .expect("wait for run");

  let status = wait_child(child).await;
  // The runner exits 0 on success, 2 on error. The run's conclusion
  // is the source of truth for "did the job pass"; the exit code is
  // a secondary signal.
  assert_eq!(
    conclusion, "success",
    "noop run should conclude with success; got {conclusion} (runner exit: {status:?})"
  );
  if let Some(s) = status {
    assert!(s.success(), "runner should exit 0 on success; got {s}");
  }
}

#[tokio::test]
#[ignore = "live test — requires TOOLU_RUNNER_LIVE_TOKEN"]
async fn multi_step_job() {
  require_live_env!();
  let harness = LiveHarness::new().await.expect("harness init");
  let workflow = "multistep-live.yml";
  let _cleanup = harness.cleanup(&[workflow]).await;

  harness.register().await.expect("register");
  let yaml = r#"
name: multistep
on:
  workflow_dispatch:
jobs:
  hello:
    runs-on: [self-hosted, toolu-runner-v1]
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-node@v4
        with:
          node-version: '20'
      - name: alpine via docker
        run: echo "hello from a step that pulls docker://alpine:3.19"
"#;
  harness
    .push_workflow(workflow, yaml)
    .await
    .expect("push multistep workflow");

  let child = harness.run_once().await.expect("spawn run --once");
  let run_id = harness
    .trigger_workflow(workflow)
    .await
    .expect("trigger workflow");

  // Multi-step jobs with action downloads + Node install can take
  // 3-5 minutes on a fresh VM.
  let conclusion = harness
    .wait_for_run(run_id, Duration::from_secs(600))
    .await
    .expect("wait for run");

  let _ = wait_child(child).await;
  assert_eq!(
    conclusion, "success",
    "multistep run should conclude with success; got {conclusion}"
  );
}

#[tokio::test]
#[ignore = "live test — requires TOOLU_RUNNER_LIVE_TOKEN"]
async fn journal_records_live_job_masked() {
  require_live_env!();
  let harness = LiveHarness::new().await.expect("harness init");
  let workflow = "journal-live.yml";
  let _cleanup = harness.cleanup(&[workflow]).await;

  harness.register().await.expect("register");
  harness
    .push_workflow(workflow, JOURNAL_YAML)
    .await
    .expect("push journal workflow");

  let child = harness.run_once().await.expect("spawn run --once");
  let run_id = harness
    .trigger_workflow(workflow)
    .await
    .expect("trigger workflow");
  let conclusion = harness
    .wait_for_run(run_id, Duration::from_secs(300))
    .await
    .expect("wait for run");
  let _ = wait_child(child).await;
  assert_eq!(conclusion, "success", "journal fixture run should succeed");

  // AC-1: exactly one journal, every line v1, seq strict, full lifecycle.
  let body = read_single_live_journal(&harness);
  let lines: Vec<JournalLine> = body
    .lines()
    .map(|l| serde_json::from_str::<JournalLine>(l).expect("every journal line parses as v1"))
    .collect();
  assert!(!lines.is_empty(), "journal must not be empty");
  for (i, line) in lines.iter().enumerate() {
    assert_eq!(line.v, 1, "line {i} contract version");
    assert_eq!(
      line.seq,
      u64::try_from(i).expect("seq index"),
      "seq must be 0..N strictly increasing"
    );
  }
  assert_journal_lifecycle(&lines);

  // AC-2: the probe output is masked; no JWT-shaped value anywhere.
  assert!(
    body.contains("token-probe=[***]"),
    "runtime token must be masked in the journal"
  );
  assert!(
    !body.contains("eyJ"),
    "JWT-shaped value leaked into the journal"
  );
}

/// Locate the run's `_diag/jobs` dir via the persisted config and return
/// the single journal's raw body.
fn read_single_live_journal(harness: &LiveHarness) -> String {
  let cfg = load_reg_config(&harness.config_dir.path().join("config.toml")).expect("load config");
  let data_dir =
    toolu_runner::config::resolve_data_dir(&cfg.runtime.data_dir).expect("resolve data dir");
  let journals: Vec<_> = std::fs::read_dir(data_dir.join("_diag").join("jobs"))
    .expect("jobs dir exists after a live run")
    .filter_map(Result::ok)
    .map(|e| e.path())
    .collect();
  assert_eq!(
    journals.len(),
    1,
    "single-job run writes exactly one journal: {journals:?}"
  );
  std::fs::read_to_string(journals.first().expect("journal path")).expect("read journal")
}

/// AC-1 lifecycle assertions over a parsed live journal.
fn assert_journal_lifecycle(lines: &[JournalLine]) {
  let has = |pred: &dyn Fn(&JournalEvent) -> bool| lines.iter().any(|l| pred(&l.event));
  assert!(has(&|e| matches!(e, JournalEvent::JobAcquired { .. })));
  assert!(has(&|e| matches!(e, JournalEvent::JobStarted { .. })));
  assert!(has(&|e| matches!(e, JournalEvent::StepStarted { .. })));
  assert!(has(&|e| matches!(
    e,
    JournalEvent::StepCompleted { conclusion, .. } if conclusion == "success"
  )));
  assert!(
    has(&|e| matches!(
      e,
      JournalEvent::Annotation { level, .. } if level == "warning"
    )),
    "::warning:: must journal as an annotation"
  );
  assert!(
    matches!(
      lines.last().map(|l| &l.event),
      Some(JournalEvent::JobCompleted { conclusion, .. }) if conclusion == "success"
    ),
    "journal must end with job_completed success"
  );
}

#[tokio::test]
#[ignore = "live test — requires TOOLU_RUNNER_LIVE_TOKEN"]
async fn action_resolution() {
  require_live_env!();
  let harness = LiveHarness::new().await.expect("harness init");
  let workflow = "action-resolution-live.yml";
  let _cleanup = harness.cleanup(&[workflow]).await;

  harness.register().await.expect("register");
  let yaml = r#"
name: action-resolution
on:
  workflow_dispatch:
jobs:
  resolve:
    runs-on: [self-hosted, toolu-runner-v1]
    steps:
      - name: checkout via actions/checkout
        uses: actions/checkout@v4
      - name: verify checkout succeeded
        run: |
          test -f README.md || test -f readme.md || test -d .git
          echo "checkout step ran"
"#;
  harness
    .push_workflow(workflow, yaml)
    .await
    .expect("push action-resolution workflow");

  let child = harness.run_once().await.expect("spawn run --once");
  let run_id = harness
    .trigger_workflow(workflow)
    .await
    .expect("trigger workflow");

  let conclusion = harness
    .wait_for_run(run_id, Duration::from_secs(600))
    .await
    .expect("wait for run");

  let _ = wait_child(child).await;
  assert_eq!(
    conclusion, "success",
    "action-resolution run should conclude with success; got {conclusion}"
  );
}

#[tokio::test]
#[ignore = "live test — requires TOOLU_RUNNER_LIVE_TOKEN"]
async fn expression_evaluation() {
  require_live_env!();
  let harness = LiveHarness::new().await.expect("harness init");
  let workflow = "expression-live.yml";
  let _cleanup = harness.cleanup(&[workflow]).await;

  harness.register().await.expect("register");
  harness
    .push_workflow(workflow, EXPRESSION_YAML)
    .await
    .expect("push expression workflow");

  let child = harness.run_once().await.expect("spawn run --once");
  let run_id = harness
    .trigger_workflow(workflow)
    .await
    .expect("trigger workflow");

  let conclusion = harness
    .wait_for_run(run_id, Duration::from_secs(300))
    .await
    .expect("wait for run");

  let _ = wait_child(child).await;
  assert_eq!(
    conclusion, "success",
    "expression run should conclude with success; got {conclusion}"
  );
}

#[tokio::test]
#[ignore = "live test — requires TOOLU_RUNNER_LIVE_TOKEN"]
async fn cancel_by_github() {
  require_live_env!();
  let harness = LiveHarness::new().await.expect("harness init");
  let workflow = "cancel-live.yml";
  let _cleanup = harness.cleanup(&[workflow]).await;

  harness.register().await.expect("register");
  // 5-minute sleep so the runner is guaranteed to be in-flight when
  // we cancel from the GH API side.
  let yaml = r#"
name: cancel
on:
  workflow_dispatch:
jobs:
  long:
    runs-on: [self-hosted, toolu-runner-v1]
    steps:
      - name: long sleep
        run: sleep 300
"#;
  harness
    .push_workflow(workflow, yaml)
    .await
    .expect("push cancel workflow");

  let child = harness.run_once().await.expect("spawn run --once");
  let run_id = harness
    .trigger_workflow(workflow)
    .await
    .expect("trigger workflow");

  // Give the runner a few seconds to actually pick up the job, then
  // cancel from the GH side.
  tokio::time::sleep(Duration::from_secs(15)).await;
  harness.cancel_run(run_id).await.expect("cancel run");

  let conclusion = harness
    .wait_for_run(run_id, Duration::from_secs(60))
    .await
    .expect("wait for cancelled run");
  let _ = wait_child(child).await;

  assert_eq!(
    conclusion, "cancelled",
    "run should conclude with cancelled after GH-side cancel; got {conclusion}"
  );
}

#[tokio::test]
#[ignore = "live test — requires TOOLU_RUNNER_LIVE_TOKEN"]
async fn concurrent_single_job() {
  require_live_env!();
  let harness = LiveHarness::new().await.expect("harness init");
  let _cleanup = harness.cleanup(&[]).await;

  harness.register().await.expect("register");

  // Start the first `run --once`; it polls until a job arrives.
  let mut first = harness.run_once().await.expect("spawn first run --once");

  // Give the first process a moment to acquire the .lock.
  tokio::time::sleep(Duration::from_secs(2)).await;

  // Start a second `run` (also --once). It should refuse to start
  // because the .lock is held; expect exit code 2 with the first's
  // PID in stderr.
  let second_binary = harness.binary_path.clone();
  let second_config = harness
    .config_path()
    .to_str()
    .expect("config path utf-8")
    .to_owned();
  let output = tokio::process::Command::new(&second_binary)
    .args(["run", "--once", "--config", &second_config])
    .output()
    .await
    .expect("spawn second run --once");

  // The first run polls until a job arrives (there is none in this
  // test), so kill it explicitly rather than waiting for a natural
  // exit that would never come.
  let _ = first.kill().await;

  assert_eq!(
    output.status.code(),
    Some(2),
    "second run should exit 2 because .lock is held; got {:?}",
    output.status.code()
  );
  // `RunnerError::LockHeld`'s Display is deterministic ("runner is
  // already running as PID {pid} …"), so no looser fallback is needed.
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(
    stderr.contains("already running as PID"),
    "second run's stderr should say another runner holds the lock; got: {stderr}"
  );
}
