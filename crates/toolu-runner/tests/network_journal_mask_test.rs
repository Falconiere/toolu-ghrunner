//! #94: a configured proxy's credentials never reach the job journal. One real
//! job runs through the engine with `[network] proxy = http://u:<pw>@…` and a
//! step echoing `$HTTPS_PROXY` and the credential's Basic form; the raw
//! (unmasked) events feed the production `observability::journal::writer`,
//! which masks through the SAME masker the run registered the proxy secrets
//! with (`job_runner` calls `ctx.add_masks(network::proxy_secrets(..))`).
//! Mirrors `secret_sink_coverage_test.rs`'s journal path.

use std::error::Error;
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use execution::execution::job_runner::run_job;
use observability::journal::writer;
use shared::{
  ActionStep, AgentJobRequestMessage, ListenerEvent, NetworkConfig, RunnerConfig, RunnerEvent,
  SecretMasker, ServicesMode,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const JOB_MESSAGE: &str = include_str!("fixtures/job_message.json");
/// An obviously-fake fixture value, not shaped like a real credential.
const PASSWORD: &str = "s3cr3tpw";
const PROXY: &str = "http://u:s3cr3tpw@proxy.corp.test:3128";

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

fn fixture_job(steps: Vec<ActionStep>) -> TestResult<AgentJobRequestMessage> {
  let mut msg: AgentJobRequestMessage = serde_json::from_str(JOB_MESSAGE)?;
  msg.steps = steps;
  Ok(msg)
}

fn test_config(dir: &std::path::Path) -> TestResult<RunnerConfig> {
  let workspace_root = dir.join("work");
  let data_dir = dir.join("data");
  std::fs::create_dir_all(&workspace_root)?;
  std::fs::create_dir_all(&data_dir)?;
  Ok(RunnerConfig {
    data_dir,
    workspace_root,
    cgroup_path: None,
    services_mode: ServicesMode::Forwarder,
    network: NetworkConfig {
      proxy: Some(PROXY.to_owned()),
      ..NetworkConfig::default()
    },
    ..RunnerConfig::default()
  })
}

/// Run the job and hand every raw event to the production journal writer;
/// returns the raw events so the test can prove the secret was really emitted.
async fn run_journaled(
  masker: Arc<Mutex<SecretMasker>>,
  jobs_dir: &std::path::Path,
  basic: &str,
) -> TestResult<Vec<RunnerEvent>> {
  let dir = tempfile::tempdir()?;
  let config = test_config(dir.path())?;
  let body = format!("echo \"proxy=$HTTPS_PROXY\"\necho \"basic={basic}\"");
  let msg = fixture_job(vec![ActionStep::script("echo-proxy", &body, "")])?;

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

  let (tx, mut rx) = mpsc::channel::<RunnerEvent>(1024);
  let forward = tokio::spawn(async move {
    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
      if jtx
        .send(ListenerEvent::Runner(event.clone()))
        .await
        .is_err()
      {
        break;
      }
      events.push(event);
    }
    events
  });

  let teardown = run_job(msg, &config, CancellationToken::new(), tx, masker).await?;
  let events = forward.await?;
  sink.await?;
  teardown.finish(&config).await;
  Ok(events)
}

#[tokio::test]
async fn proxy_credentials_never_reach_the_journal() -> TestResult {
  let basic = base64::engine::general_purpose::STANDARD.encode(format!("u:{PASSWORD}"));
  // The SAME masker the run registers the proxy secrets with.
  let masker = Arc::new(Mutex::new(SecretMasker::new()));
  let jobs_dir = tempfile::tempdir()?;

  let events = run_journaled(Arc::clone(&masker), jobs_dir.path(), &basic).await?;

  // Non-vacuous: the engine really emitted the raw credentials (the event
  // stream is unmasked by design).
  let raw_lines: Vec<&str> = events
    .iter()
    .filter_map(|event| {
      if let RunnerEvent::Log { line, .. } = event {
        Some(line.as_str())
      } else {
        None
      }
    })
    .collect();
  assert!(
    raw_lines.iter().any(|line| line.contains(PASSWORD)),
    "engine never emitted the proxy password: {raw_lines:?}"
  );

  let mut files = std::fs::read_dir(jobs_dir.path())?
    .collect::<Result<Vec<_>, _>>()?
    .into_iter()
    .map(|entry| entry.path())
    .collect::<Vec<_>>();
  let journal_path = files.pop().ok_or("expected exactly one journal file")?;
  assert!(files.is_empty(), "expected exactly one journal file");
  let journal = std::fs::read_to_string(&journal_path)?;

  assert!(
    !journal.contains(PASSWORD),
    "proxy password leaked into the journal"
  );
  assert!(
    !journal.contains(&basic),
    "base64(user:password) leaked into the journal"
  );
  assert!(
    journal.contains("proxy=http://***@proxy.corp.test:3128"),
    "masked proxy URL missing from the journal: {journal}"
  );
  Ok(())
}
