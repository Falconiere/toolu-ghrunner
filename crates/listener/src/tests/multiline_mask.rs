//! Issue #86: real shell/job dispatch through production masking sinks.
//! The sanitized message envelope is reused; only its steps are replaced with
//! local disposable probes. No GitHub service or successful response is mocked.

use std::io::Write;

use super::*;
use execution::execution::job_runner::run_job;
use observability::journal::{types::JournalLine, writer};
use shared::startup::{RedactingWriter, SecretRedactor};
use shared::{ActionStep, MaskerRedactor, RunnerConfig, TemplateToken};

const SCRIPT: &str = include_str!("../../tests/multiline_mask.sh");
const WHOLE: &str = " Q \nRS\r\n TUV \r probe-overlap \nprobe-overlap-tail\r\nprobe-overlap\n\n";
const PROBES: [&str; 7] = [
  "Q",
  "RS",
  "TUV",
  "probe-overlap",
  "probe-overlap-tail",
  "probe%0Aliteral",
  "Ω",
];
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn fixture(config: &RunnerConfig) -> TestResult<AgentJobRequestMessage> {
  let mut msg: AgentJobRequestMessage = serde_json::from_str(include_str!(
    "../../../toolu-runner/tests/fixtures/job_message.json"
  ))?;
  let workspace = config.workspace_root.join(&msg.job_id);
  for name in ["outer", "inner"] {
    std::fs::create_dir_all(workspace.join(name))?;
  }
  std::fs::write(workspace.join("probe.sh"), SCRIPT)?;
  std::fs::write(
    workspace.join("outer/action.yml"),
    "name: outer\ndescription: masking probe\nruns:\n  using: composite\n  steps:\n    - uses: ./inner\n",
  )?;
  std::fs::write(
    workspace.join("inner/action.yml"),
    "name: inner\ndescription: masking probe\nruns:\n  using: composite\n  steps:\n    - shell: bash\n      run: bash probe.sh\n",
  )?;
  // Keep the captured wire step UUID/contextName distinction and token shapes.
  let mut register = msg.steps.first().ok_or("fixture missing script")?.clone();
  register.inputs = ActionStep::script("unused", "bash probe.sh register", "").inputs;
  let mut nested = msg.steps.get(1).ok_or("fixture missing action")?.clone();
  nested.reference.name = Some("./outer".to_owned());
  nested.reference.git_ref = None;
  nested.reference.repository_type = Some("Local".to_owned());
  nested.inputs = TemplateToken::default();
  let annotation = ActionStep::script(
    "33333333-aaaa-4bbb-8ccc-ffffffffffff",
    "printf '%s\\n' '::notice::annotation=[Q|RS|TUV]'",
    "",
  );
  msg.steps = vec![register, nested, annotation];
  Ok(msg)
}

fn forwarder(masker: Arc<Mutex<SecretMasker>>, live_tx: mpsc::Sender<LiveLogLine>) -> FwdConfig {
  FwdConfig {
    results_url: None,
    results_client: reqwest::Client::new(),
    results_token: String::new(),
    run_backend_id: String::new(),
    job_backend_id: String::new(),
    setup_lines: Vec::new(),
    live_log_tx: Some(live_tx),
    masker,
  }
}

fn assert_probe_lines(lines: &[String]) {
  let mut probes: Vec<_> = lines
    .iter()
    .filter(|line| line.starts_with("stdout=[") || line.starts_with("stderr=["))
    .cloned()
    .collect();
  probes.sort();
  let mut expected = vec!["stdout=[***]".to_owned(); PROBES.len()];
  expected.extend(vec!["stderr=[***]".to_owned(); PROBES.len()]);
  expected.sort();
  assert_eq!(probes, expected);
  assert!(lines.iter().any(|line| line == "control remains visible"));
}

#[tokio::test]
async fn multiline_mask_real_job_reaches_every_local_sink() -> TestResult {
  let temp = tempfile::tempdir()?;
  let config = RunnerConfig {
    workspace_root: temp.path().join("work"),
    data_dir: temp.path().join("data"),
    ..RunnerConfig::default()
  };
  let msg = fixture(&config)?;
  let job_id = msg.job_id.clone();
  let masker = Arc::new(Mutex::new(SecretMasker::new()));
  let (live_tx, mut live_rx) = mpsc::channel(512);
  let cfg = forwarder(Arc::clone(&masker), live_tx);
  let mut state = ForwarderState::new(Vec::new(), &cfg);
  let (step_tx, mut step_rx) = mpsc::channel(512);
  let (journal_tx, journal_rx) = mpsc::channel(512);
  let jobs_dir = temp.path().join("journal");
  let journal = writer::spawn(journal_rx, jobs_dir.clone(), Arc::clone(&masker));
  journal_tx
    .send(ListenerEvent::JobAcquired {
      job_id,
      run_service_url: String::new(),
    })
    .await?;
  let redactor: Arc<dyn SecretRedactor> = Arc::new(MaskerRedactor(Arc::clone(&masker)));
  let mut diag = Vec::new();
  let mut diagnostic = RedactingWriter::new(&mut diag, redactor);
  let (tx, mut rx) = mpsc::channel(512);
  let job_config = config.clone();
  let job_masker = Arc::clone(&masker);
  let job = tokio::spawn(async move {
    run_job(msg, &job_config, CancellationToken::new(), tx, job_masker).await
  });
  let mut stdout = 0;
  let mut stderr = 0;
  let mut annotations = 0;
  let mut succeeded = false;
  while let Some(event) = rx.recv().await {
    if let RunnerEvent::Log {
      step_id,
      line,
      stream,
    } = &event
    {
      if line.starts_with("stdout=[") {
        assert_eq!(*stream, shared::LogStream::Stdout);
        stdout += 1;
      }
      if line.starts_with("stderr=[") {
        assert_eq!(*stream, shared::LogStream::Stderr);
        stderr += 1;
      }
      state
        .uploaders
        .entry(step_id.clone())
        .or_insert_with(|| step_tx.clone());
      handle_event_arm(&mut state, &cfg, &event).await;
      // Diagnostics normally receive tracing records, not stdout. Exercise the
      // actual sink writer/redactor with each raw line after live registration.
      writeln!(diagnostic, "{line}")?;
    }
    if let RunnerEvent::Annotation { message, .. } = &event {
      assert_eq!(message, "annotation=[***|***|***]");
      annotations += 1;
    }
    if let RunnerEvent::JobCompleted { conclusion, .. } = &event {
      assert_eq!(*conclusion, Conclusion::Success);
      succeeded = true;
    }
    journal_tx.send(ListenerEvent::Runner(event)).await?;
  }
  job.await??.finish(&config).await;
  drop(journal_tx);
  journal.await?;
  diagnostic.flush()?;
  drop(diagnostic);
  assert!(succeeded);
  assert_eq!(
    (stdout, stderr, annotations),
    (PROBES.len(), PROBES.len(), 1)
  );
  assert_probe_lines(&state.all_job_lines);
  let mut step_lines = Vec::new();
  while let Ok(line) = step_rx.try_recv() {
    step_lines.push(line);
  }
  let mut live_lines = Vec::new();
  while let Ok(line) = live_rx.try_recv() {
    live_lines.push(line.line);
  }
  assert_eq!(step_lines, state.all_job_lines);
  assert_eq!(live_lines, state.all_job_lines);
  let diagnostic_lines: Vec<_> = String::from_utf8(diag)?
    .lines()
    .map(str::to_owned)
    .collect();
  assert_eq!(diagnostic_lines, state.all_job_lines);
  let mut journal_lines = Vec::new();
  for entry in std::fs::read_dir(jobs_dir)? {
    let text = std::fs::read_to_string(entry?.path())?;
    for line in text.lines() {
      let entry: JournalLine = serde_json::from_str(line)?;
      if let observability::journal::types::JournalEvent::Log { line, .. } = entry.event {
        journal_lines.push(line);
      }
    }
  }
  assert_eq!(journal_lines, state.all_job_lines);
  let guard = masker.lock().map_err(|e| e.to_string())?;
  assert_eq!(guard.mask(WHOLE), "***", "preserve the exact whole value");
  assert_eq!(
    guard.mask(" "),
    " ",
    "empty lines must not register whitespace"
  );
  for probe in PROBES {
    assert_eq!(guard.mask(probe), "***", "missing line: {probe}");
  }
  Ok(())
}
