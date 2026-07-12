//! Journal writer driven by a REAL engine job (`run_job` over the committed
//! `job_message.json` fixture — no mocks): line contract, secret masking,
//! retention pruning (AC-7), unwritable-dir resilience (AC-6), and the
//! env-gated canonical fixture capture (`JOURNAL_CAPTURE=1`).

use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use shared::SecretMasker;
use shared::{ActionStep, AgentJobRequestMessage, ListenerEvent, RunnerConfig, ServicesMode};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use execution::execution::job_runner::run_job;
use observability::journal::{JOURNAL_RETAIN, JournalEvent, JournalLine, writer};

const JOB_MESSAGE: &str = include_str!("fixtures/job_message.json");

type TestResult<T = ()> = Result<T, Box<dyn Error>>;
type SharedMasker = Arc<Mutex<SecretMasker>>;

fn fixture_job(steps: Vec<ActionStep>) -> TestResult<AgentJobRequestMessage> {
  let mut msg: AgentJobRequestMessage = serde_json::from_str(JOB_MESSAGE)?;
  msg.steps = steps;
  Ok(msg)
}

/// Run a real engine job and journal its listener event stream into
/// `jobs_dir`, mirroring the production wiring (session + acquire prelude,
/// then every engine event wrapped as `ListenerEvent::Runner`).
async fn run_journaled_job(
  steps: Vec<ActionStep>,
  jobs_dir: &Path,
  masker: SharedMasker,
) -> TestResult {
  let dir = tempfile::tempdir()?;
  let workspace_root = dir.path().join("work");
  let data_dir = dir.path().join("data");
  std::fs::create_dir_all(&workspace_root)?;
  std::fs::create_dir_all(&data_dir)?;
  let config = RunnerConfig {
    data_dir,
    workspace_root,
    cgroup_path: None,
    services_mode: ServicesMode::Forwarder,
    ..RunnerConfig::default()
  };

  let msg = fixture_job(steps)?;
  let (jtx, jrx) = mpsc::channel::<ListenerEvent>(256);
  let sink = writer::spawn(jrx, jobs_dir.to_path_buf(), Arc::clone(&masker));

  jtx
    .send(ListenerEvent::SessionCreated {
      session_id: "00000000-0000-0000-0000-000000000000".to_owned(),
    })
    .await?;
  jtx
    .send(ListenerEvent::JobAcquired {
      job_id: msg.job_id.clone(),
      run_service_url: "https://run.example".to_owned(),
    })
    .await?;

  let (tx, mut rx) = mpsc::channel(1024);
  let fwd = tokio::spawn(async move {
    while let Some(ev) = rx.recv().await {
      if jtx.send(ListenerEvent::Runner(ev)).await.is_err() {
        break;
      }
    }
  });

  run_job(msg, &config, CancellationToken::new(), tx, masker).await?;
  fwd.await?;
  sink.await?;
  Ok(())
}

/// Parse every line of the single journal file in `jobs_dir`.
fn read_single_journal(jobs_dir: &Path) -> TestResult<(PathBuf, Vec<JournalLine>)> {
  let mut files: Vec<PathBuf> = std::fs::read_dir(jobs_dir)?
    .collect::<Result<Vec<_>, _>>()?
    .into_iter()
    .map(|e| e.path())
    .collect();
  assert_eq!(
    files.len(),
    1,
    "expected exactly one journal, got {files:?}"
  );
  let path = files.remove(0);
  let body = std::fs::read_to_string(&path)?;
  let lines = body
    .lines()
    .map(serde_json::from_str::<JournalLine>)
    .collect::<Result<Vec<_>, _>>()?;
  Ok((path, lines))
}

/// `<yyyymmdd>T<hhmmss>Z-<sanitized id>.jsonl` without a regex dep.
fn name_matches_contract(name: &str) -> bool {
  let Some(rest) = name.strip_suffix(".jsonl") else {
    return false;
  };
  let Some((ts, id)) = rest.split_once('-') else {
    return false;
  };
  let ts_ok = ts.len() == 16
    && ts.chars().enumerate().all(|(i, c)| match i {
      8 => c == 'T',
      15 => c == 'Z',
      _ => c.is_ascii_digit(),
    });
  let id_ok = !id.is_empty()
    && id
      .chars()
      .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
  ts_ok && id_ok
}

#[test]
fn jobs_dir_derives_from_data_dir() {
  let dir = writer::jobs_dir_for(Path::new("/var/lib/toolu-runner"));
  assert_eq!(dir, Path::new("/var/lib/toolu-runner/_diag/jobs"));
}

#[tokio::test]
async fn journal_matches_contract_for_real_job() -> TestResult {
  let jobs_dir = tempfile::tempdir()?;
  let masker = Arc::new(Mutex::new(SecretMasker::new()));
  let steps = vec![ActionStep::script("echo", "echo one; echo two", "")];
  run_journaled_job(steps, jobs_dir.path(), masker).await?;

  let (path, lines) = read_single_journal(jobs_dir.path())?;
  let name = path
    .file_name()
    .and_then(|n| n.to_str())
    .unwrap_or_default();
  assert!(name_matches_contract(name), "bad journal name: {name}");

  for (i, line) in lines.iter().enumerate() {
    assert_eq!(line.v, 1, "line {i} has wrong version");
    assert_eq!(line.seq, i as u64, "seq must be 0..N strictly increasing");
  }
  assert!(
    matches!(
      lines.first().map(|l| &l.event),
      Some(JournalEvent::SessionCreated { .. })
    ),
    "first event must be the buffered session_created"
  );
  assert!(matches!(
    lines.get(1).map(|l| &l.event),
    Some(JournalEvent::JobAcquired { .. })
  ));
  let has = |pred: &dyn Fn(&JournalEvent) -> bool| lines.iter().any(|l| pred(&l.event));
  assert!(has(&|e| matches!(e, JournalEvent::JobStarted { .. })));
  assert!(has(&|e| matches!(e, JournalEvent::StepStarted { .. })));
  assert!(has(&|e| matches!(
    e,
    JournalEvent::StepCompleted { conclusion, .. } if conclusion == "success"
  )));
  assert!(has(&|e| matches!(
    e,
    JournalEvent::JobCompleted { conclusion, .. } if conclusion == "success"
  )));
  assert!(has(&|e| matches!(
    e,
    JournalEvent::Log { line, .. } if line.contains("one")
  )));
  Ok(())
}

#[tokio::test]
async fn registered_secrets_never_reach_the_journal() -> TestResult {
  let jobs_dir = tempfile::tempdir()?;
  let masker = Arc::new(Mutex::new(SecretMasker::new()));
  let secret = "hush-s3cr3t-value-8f2a";
  masker
    .lock()
    .map_err(|e| format!("masker lock poisoned: {e}"))?
    .add_secret(secret);
  let body = format!("echo leaking {secret} now");
  let steps = vec![ActionStep::script("leak", &body, "")];
  run_journaled_job(steps, jobs_dir.path(), masker).await?;

  let (path, _) = read_single_journal(jobs_dir.path())?;
  let raw = std::fs::read_to_string(path)?;
  assert!(
    !raw.contains(secret),
    "registered secret leaked into the journal"
  );
  Ok(())
}

#[tokio::test]
async fn retention_prunes_oldest_to_cap() -> TestResult {
  let jobs_dir = tempfile::tempdir()?;
  for i in 0..JOURNAL_RETAIN {
    let name = format!("20200101T{:06}Z-old-{i}.jsonl", i);
    std::fs::write(jobs_dir.path().join(name), "{}\n")?;
  }
  let masker = Arc::new(Mutex::new(SecretMasker::new()));
  let steps = vec![ActionStep::script("echo", "echo hi", "")];
  run_journaled_job(steps, jobs_dir.path(), masker).await?;

  let mut names: Vec<String> = std::fs::read_dir(jobs_dir.path())?
    .collect::<Result<Vec<_>, _>>()?
    .into_iter()
    .filter_map(|e| e.file_name().into_string().ok())
    .collect();
  names.sort();
  assert_eq!(
    names.len(),
    JOURNAL_RETAIN,
    "prune must cap the dir at the limit"
  );
  // The pre-seeded files are lexicographically ordered by their zero-padded
  // timestamp; the single oldest (`-old-0`) is the one pruned.
  assert!(
    !names.contains(&"20200101T000000Z-old-0.jsonl".to_owned()),
    "the single oldest pre-seeded file must be pruned; got {names:?}"
  );
  assert!(
    names.contains(&"20200101T000001Z-old-1.jsonl".to_owned()),
    "the second-oldest file must survive; got {names:?}"
  );
  assert!(
    names.last().is_some_and(|n| !n.starts_with("20200101")),
    "the new journal must survive the prune"
  );
  Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn unwritable_jobs_dir_never_fails_the_job() -> TestResult {
  use std::os::unix::fs::PermissionsExt;
  let jobs_dir = tempfile::tempdir()?;
  std::fs::set_permissions(jobs_dir.path(), std::fs::Permissions::from_mode(0o555))?;
  // Root ignores mode bits; probe and skip in that case.
  if std::fs::write(jobs_dir.path().join(".probe"), b"x").is_ok() {
    eprintln!("skipping: running as root, cannot make dir unwritable");
    return Ok(());
  }

  let masker = Arc::new(Mutex::new(SecretMasker::new()));
  let steps = vec![ActionStep::script("echo", "echo unaffected", "")];
  // The job itself must complete; the writer WARNs once and goes quiet.
  run_journaled_job(steps, jobs_dir.path(), masker).await?;

  let count = std::fs::read_dir(jobs_dir.path())?.count();
  assert_eq!(count, 0, "no journal file may exist in an unwritable dir");
  Ok(())
}

/// Env-gated capture: writes the canonical real-engine fixture used by the
/// reader/state tests. Run once via
/// `JOURNAL_CAPTURE=1 cargo test -p toolu-runner --test journal_writer_test capture_canonical`.
#[tokio::test]
async fn capture_canonical_fixture() -> TestResult {
  if std::env::var("JOURNAL_CAPTURE").is_err() {
    return Ok(());
  }
  let jobs_dir = tempfile::tempdir()?;
  let masker = Arc::new(Mutex::new(SecretMasker::new()));
  let steps = vec![
    ActionStep::script(
      "greet",
      "echo hello from step one\necho \"::warning file=demo.txt,line=3::deprecated feature\"",
      "",
    ),
    ActionStep::script("farewell", "echo done from step two", ""),
  ];
  run_journaled_job(steps, jobs_dir.path(), masker).await?;

  let (path, lines) = read_single_journal(jobs_dir.path())?;
  assert!(!lines.is_empty(), "captured journal is empty");
  let dest = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/journal/canonical.jsonl");
  if let Some(parent) = dest.parent() {
    std::fs::create_dir_all(parent)?;
  }
  std::fs::copy(&path, &dest)?;
  eprintln!("canonical fixture written to {}", dest.display());
  Ok(())
}
