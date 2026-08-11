//! AC-5 (T4 sink coverage): after `command_dispatch.rs`'s engine-side stdout
//! mask is deleted, the engine's `RunnerEvent` stream is emitted UNMASKED
//! (see `Runner::execute_job`'s doc contract). This test drives one real job
//! whose step echoes a REGISTERED secret and proves that secret still never
//! reaches any of the four durable sinks named in the design's AC-5:
//!
//! 1. the combined job log,
//! 2. the per-step upload buffer,
//! 3. the journal JSONL under `_diag/jobs/`,
//! 4. `_diag/runner.log`.
//!
//! ## What's real vs. reconstructed
//!
//! The job itself is driven through the real engine
//! (`execution::execution::job_runner::run_job`), exactly like
//! `journal_writer_test.rs` / `gh_compat_forwarder.rs`.
//!
//! Sink 3 (the journal) is fully real: the raw, unmasked `RunnerEvent`s are
//! wrapped as `ListenerEvent::Runner` and handed to the production
//! `observability::journal::writer::spawn`, which does its OWN internal
//! masking — nothing about the masking step is reimplemented here.
//!
//! Sinks 1 and 2 (the combined job log and the per-step upload buffer) are
//! assembled by `listener::execution_loop::forward_log_line` /
//! `ForwarderState`, both `pub(super)` to the `listener` crate and therefore
//! unreachable from an external `toolu-runner` integration test. What IS
//! reachable — and genuinely exercised here — is the real, `pub`
//! `listener::log_uploader::{upload_job_logs, upload_compressed_step_logs}`,
//! the actual functions that gzip-encode and HTTP-PUT those two sinks to the
//! Results Service. This test masks each `Log` line through the SAME shared
//! masker, mirroring (not calling) `forward_log_line`'s masking step, then
//! feeds the result into those real upload functions against a real local
//! `wiremock` server, and inspects the ACTUAL bytes PUT to that server. So
//! the private event-to-buffer glue is mirrored here; the sink-writing code
//! is not.
//!
//! That mirror does NOT prove `forward_log_line` itself calls the masker —
//! deleting its `mask_line` call would leave this test green, since the
//! masking done here happens independently of production code. That gap is
//! closed by a separate in-crate test,
//! `listener::execution_loop::tests::forward_log_line_masks_both_the_job_log_and_the_step_upload_channel`,
//! which calls the REAL `forward_log_line` directly (it is `pub(super)`,
//! so — like `helpers.rs`'s existing inline tests — it has to live inside
//! `listener/src/execution_loop.rs`, not here) and asserts both
//! `state.all_job_lines` and the per-step upload channel come out masked.
//! So today: sinks 3 and 4 are driven end-to-end by THIS file against real
//! production code; sinks 1 and 2 are driven end-to-end by the real
//! `forward_log_line` in the in-crate `listener` test, while this file
//! drives their real downstream upload/HTTP code from a masked-by-mirror
//! input.
//!
//! Sink 4 (`_diag/runner.log`) is the tracing file sink
//! (`shared::startup::init_with_redactor`). Its path is hardcoded to
//! `$HOME/.toolu-runner`, so this test redirects `HOME` for just the
//! duration of that one synchronous call via `temp_env::with_var` — the same
//! sanctioned, serialized-behind-a-lock mechanism `auth_store_test.rs` uses,
//! safe here because this is the only test in this binary that touches
//! `HOME`. By inspection, normal job stdout never reaches a `tracing::*!`
//! call (the `RunnerEvent::Log` stream flows only through `mpsc` channels),
//! so proving this sink non-vacuously requires directly issuing one
//! representative tracing event carrying the raw secret — simulating the
//! realistic threat (some future log statement interpolating a secret) that
//! `MaskerRedactor` exists to guard against. The real job also runs under
//! this same subscriber, so its own (secret-free) tracing output is folded
//! into the same assertion.
//!
//! This test does not drive the full `GitHubListener::run()` broker
//! lifecycle (JIT auth, encrypted message decrypt, run-service renewal) —
//! that would require fabricating a complete fake broker protocol
//! independent of what T4 changed. It targets exactly the sink-coverage
//! question T4 raises, at the lowest layer that still exercises each sink's
//! real, production sink-writing code.

use std::collections::HashMap;
use std::error::Error;
use std::io::Read;
use std::sync::{Arc, Mutex};

use execution::execution::job_runner::run_job;
use listener::helpers::ResultsCtx;
use listener::log_uploader::{upload_compressed_step_logs, upload_job_logs};
use observability::journal::writer;
use serde_json::json;
use shared::startup::SecretRedactor;
use shared::{
  ActionStep, AgentJobRequestMessage, ListenerEvent, MaskerRedactor, RunnerConfig, RunnerEvent,
  SecretMasker, ServicesMode,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const JOB_MESSAGE: &str = include_str!("fixtures/job_message.json");
/// An obviously-fake fixture value, not shaped like a real credential.
const SECRET: &str = "s3cr3t-sink-coverage-9f21ab";
/// Twirp service path for the Results Service log/summary RPCs.
const RESULTS_RECEIVER_SERVICE: &str = "/twirp/results.services.receiver.Receiver/";

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

/// Load the committed fixture and swap in `steps`.
fn fixture_job(steps: Vec<ActionStep>) -> TestResult<AgentJobRequestMessage> {
  let mut msg: AgentJobRequestMessage = serde_json::from_str(JOB_MESSAGE)?;
  msg.steps = steps;
  Ok(msg)
}

/// Build a throwaway `RunnerConfig` rooted under `dir` with its `work`/`data`
/// dirs created.
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
    ..RunnerConfig::default()
  })
}

/// Send the buffered session/acquire prelude the journal expects before any
/// engine event, mirroring `journal_writer_test.rs::run_journaled_job`.
async fn prime_journal(jtx: &mpsc::Sender<ListenerEvent>, job_id: &str) -> TestResult {
  jtx
    .send(ListenerEvent::SessionCreated {
      session_id: "00000000-0000-0000-0000-000000000000".to_owned(),
    })
    .await?;
  jtx
    .send(ListenerEvent::JobAcquired {
      job_id: job_id.to_owned(),
      run_service_url: "https://run.example".to_owned(),
    })
    .await?;
  Ok(())
}

/// Drive one real job (a step that echoes `SECRET`, already registered with
/// `masker`) through the real engine, mirroring the journal-forwarding shape
/// of `journal_writer_test.rs::run_journaled_job`. Returns every emitted
/// (still-unmasked, per T4) `RunnerEvent`; the journal received the same
/// events live, masked internally by the real writer.
async fn run_and_collect(
  masker: Arc<Mutex<SecretMasker>>,
  jobs_dir: &std::path::Path,
) -> TestResult<Vec<RunnerEvent>> {
  let dir = tempfile::tempdir()?;
  let config = test_config(dir.path())?;

  let body = format!("echo leaking {SECRET} now");
  let msg = fixture_job(vec![ActionStep::script("leak", &body, "")])?;

  let (jtx, jrx) = mpsc::channel::<ListenerEvent>(256);
  let sink = writer::spawn(jrx, jobs_dir.to_path_buf(), Arc::clone(&masker));
  prime_journal(&jtx, &msg.job_id).await?;

  let (tx, mut rx) = mpsc::channel::<RunnerEvent>(1024);
  let fwd = tokio::spawn(async move {
    let mut events = Vec::new();
    while let Some(ev) = rx.recv().await {
      if jtx.send(ListenerEvent::Runner(ev.clone())).await.is_err() {
        break;
      }
      events.push(ev);
    }
    events
  });

  let teardown = run_job(msg, &config, CancellationToken::new(), tx, masker).await?;
  let events = fwd.await?;
  sink.await?;
  // `run_job` now defers cache maintenance + workspace GC to the caller.
  teardown.finish(&config).await;
  Ok(events)
}

/// Mask one line through the shared masker, mirroring
/// `listener::execution_loop::forward_log_line`'s (`pub(super)`, hence
/// unreachable here) one-line `mask_line` helper verbatim.
fn mask_line(masker: &Arc<Mutex<SecretMasker>>, line: &str) -> String {
  match masker.lock() {
    Ok(g) => g.mask(line).into_owned(),
    Err(poisoned) => poisoned.into_inner().mask(line).into_owned(),
  }
}

/// Rebuild the combined job log and the per-step buffers the way the
/// (private-to-`listener`) forwarder would, so the real, `pub` upload
/// functions can be driven with production-shaped, already-masked input.
fn masked_log_buffers(
  events: &[RunnerEvent],
  masker: &Arc<Mutex<SecretMasker>>,
) -> (Vec<String>, HashMap<String, Vec<String>>) {
  let mut all_job_lines = Vec::new();
  let mut per_step: HashMap<String, Vec<String>> = HashMap::new();
  for event in events {
    if let RunnerEvent::Log { step_id, line, .. } = event {
      let redacted = mask_line(masker, line);
      all_job_lines.push(redacted.clone());
      per_step.entry(step_id.clone()).or_default().push(redacted);
    }
  }
  (all_job_lines, per_step)
}

/// Mount the minimal real Results Service log-upload surface: signed-URL
/// request, blob PUT, metadata finalize — for both the job- and step-level
/// RPCs `upload_job_logs` / `upload_compressed_step_logs` call.
async fn mount_results_service(server: &MockServer) {
  Mock::given(method("POST"))
    .and(path(format!(
      "{RESULTS_RECEIVER_SERVICE}GetJobLogsSignedBlobURL"
    )))
    .respond_with(ResponseTemplate::new(200).set_body_json(json!({
      "logs_url": format!("{}/blob/job", server.uri()),
      "blob_storage_type": "BLOB_STORAGE_TYPE_AZURE",
    })))
    .mount(server)
    .await;
  Mock::given(method("PUT"))
    .and(path("/blob/job"))
    .respond_with(ResponseTemplate::new(201))
    .mount(server)
    .await;
  Mock::given(method("POST"))
    .and(path(format!(
      "{RESULTS_RECEIVER_SERVICE}CreateJobLogsMetadata"
    )))
    .respond_with(ResponseTemplate::new(200))
    .mount(server)
    .await;

  Mock::given(method("POST"))
    .and(path(format!(
      "{RESULTS_RECEIVER_SERVICE}GetStepLogsSignedBlobURL"
    )))
    .respond_with(ResponseTemplate::new(200).set_body_json(json!({
      "logs_url": format!("{}/blob/step", server.uri()),
      "blob_storage_type": "BLOB_STORAGE_TYPE_AZURE",
    })))
    .mount(server)
    .await;
  Mock::given(method("PUT"))
    .and(path("/blob/step"))
    .respond_with(ResponseTemplate::new(201))
    .mount(server)
    .await;
  Mock::given(method("POST"))
    .and(path(format!(
      "{RESULTS_RECEIVER_SERVICE}CreateStepLogsMetadata"
    )))
    .respond_with(ResponseTemplate::new(200))
    .mount(server)
    .await;
}

/// Every gzip blob body actually PUT to `server` (the real bytes the
/// production `upload_log_blob` sent over the wire).
async fn captured_blob_bodies(server: &MockServer) -> TestResult<Vec<Vec<u8>>> {
  let received = server
    .received_requests()
    .await
    .ok_or("mock server request recording was disabled")?;
  Ok(
    received
      .into_iter()
      .filter(|r| r.method.as_str() == "PUT")
      .map(|r| r.body)
      .collect(),
  )
}

/// Decompress a gzip blob body into text.
fn gunzip(bytes: &[u8]) -> TestResult<String> {
  let mut decoder = flate2::read::GzDecoder::new(bytes);
  let mut out = String::new();
  decoder.read_to_string(&mut out)?;
  Ok(out)
}

/// Install the real `init_with_redactor` file-sink wiring (the exact
/// production tracing setup) pointed at a temp `$HOME` instead of the real
/// one, sharing `masker` with the job. `HOME` is only overridden for the
/// duration of this one synchronous call.
fn init_file_sink(masker: Arc<Mutex<SecretMasker>>) -> TestResult<tempfile::TempDir> {
  let tmp_home = tempfile::tempdir()?;
  let home = tmp_home
    .path()
    .to_str()
    .ok_or("temp home path is not valid UTF-8")?
    .to_owned();
  let redactor: Arc<dyn SecretRedactor> = Arc::new(MaskerRedactor(masker));
  let init_result: Result<(), shared::RunnerError> =
    temp_env::with_var("HOME", Some(home.as_str()), || {
      shared::startup::init_with_redactor(&home, "runner", redactor)
    });
  init_result?;
  Ok(tmp_home)
}

/// Concatenate every `runner.log*` file under `<tmp_home>/.toolu-runner/_diag/`
/// (`tracing_appender`'s daily rotation suffixes even today's active file
/// with the date, e.g. `runner.log.2026-08-06`).
fn read_runner_log(tmp_home: &tempfile::TempDir) -> TestResult<String> {
  let diag = tmp_home.path().join(".toolu-runner").join("_diag");
  let mut combined = String::new();
  for entry in std::fs::read_dir(&diag)? {
    let entry = entry?;
    let name = entry.file_name();
    if name.to_str().is_some_and(|n| n.starts_with("runner.log")) {
      combined.push_str(&std::fs::read_to_string(entry.path())?);
    }
  }
  Ok(combined)
}

/// Sink 3: the journal JSONL — fully real, no reconstruction (see module
/// doc). Asserts no raw secret and a genuine mask marker (non-vacuous).
fn assert_journal_clean(jobs_dir: &std::path::Path) -> TestResult {
  let mut journal_files: Vec<std::path::PathBuf> = std::fs::read_dir(jobs_dir)?
    .collect::<Result<Vec<_>, _>>()?
    .into_iter()
    .map(|e| e.path())
    .collect();
  let journal_path = journal_files
    .pop()
    .ok_or("expected exactly one journal file")?;
  let journal_raw = std::fs::read_to_string(&journal_path)?;
  assert!(
    !journal_raw.contains(SECRET),
    "secret leaked into the journal: {}",
    journal_path.display()
  );
  assert!(
    journal_raw.contains("***"),
    "journal missing the expected mask marker — the assertion above would be vacuous"
  );
  Ok(())
}

/// Sink 1 shape check: the combined job log must be non-empty and contain
/// the echo line already masked (non-vacuous — proves lines were captured
/// at all before the upload/decompress round-trip is even attempted).
fn assert_job_log_has_masked_line(all_job_lines: &[String]) {
  assert!(
    !all_job_lines.is_empty(),
    "no Log events were captured; the sink assertions below would be vacuous"
  );
  assert!(
    all_job_lines
      .iter()
      .any(|l| l.contains("leaking") && l.contains("***")),
    "expected a masked echo line in the combined job log; got {all_job_lines:?}"
  );
}

/// Drive the REAL `upload_job_logs` / `upload_compressed_step_logs` against
/// a real local server and return every gzip blob body it actually PUT.
async fn upload_and_capture_blobs(
  all_job_lines: &[String],
  per_step: &HashMap<String, Vec<String>>,
) -> TestResult<Vec<Vec<u8>>> {
  let server = MockServer::start().await;
  mount_results_service(&server).await;
  let client = reqwest::Client::new();
  let rctx = ResultsCtx {
    client: &client,
    results_url: &server.uri(),
    token: "test-token",
    run_backend_id: "run-1",
    job_backend_id: "job-1",
  };

  let job_upload = upload_job_logs(&rctx, all_job_lines).await;
  assert!(
    job_upload.is_some(),
    "combined job log upload did not complete against the mock Results Service"
  );
  for (step_id, lines) in per_step {
    let step_upload = upload_compressed_step_logs(&rctx, step_id, lines).await;
    assert!(
      step_upload.is_some(),
      "per-step log upload did not complete for step {step_id}"
    );
  }
  captured_blob_bodies(&server).await
}

/// Decompress every captured blob and assert none carries the raw secret,
/// while confirming each was genuinely masked (non-vacuous).
fn assert_blobs_clean(blobs: &[Vec<u8>]) -> TestResult {
  assert!(
    !blobs.is_empty(),
    "no blob PUT was captured; the sink assertions below would be vacuous"
  );
  for blob in blobs {
    let text = gunzip(blob)?;
    assert!(
      !text.contains(SECRET),
      "secret leaked into an uploaded log blob: {text}"
    );
    assert!(
      text.contains("***"),
      "uploaded blob missing the expected mask marker: {text}"
    );
  }
  Ok(())
}

/// Sinks 1 & 2: rebuild the combined job log and per-step buffers (see
/// module doc), then prove the real upload path never carries the secret.
async fn assert_upload_sinks_clean(
  events: &[RunnerEvent],
  masker: &Arc<Mutex<SecretMasker>>,
) -> TestResult {
  let (all_job_lines, per_step) = masked_log_buffers(events, masker);
  assert_job_log_has_masked_line(&all_job_lines);
  let blobs = upload_and_capture_blobs(&all_job_lines, &per_step).await?;
  assert_blobs_clean(&blobs)
}

/// Sink 4: `_diag/runner.log`. Real job stdout never reaches a
/// `tracing::*!` call (confirmed by inspection — see module doc), so this
/// directly issues one event carrying the raw secret to non-vacuously
/// exercise the real `init_with_redactor` writer/redactor wiring.
fn assert_runner_log_clean(tmp_home: &tempfile::TempDir) -> TestResult {
  tracing::error!(
    leaked_field = %SECRET,
    "diagnostic line simulating a future log statement carrying a raw secret"
  );
  let runner_log = read_runner_log(tmp_home)?;
  assert!(
    !runner_log.contains(SECRET),
    "secret leaked into _diag/runner.log"
  );
  assert!(
    runner_log.contains("***"),
    "_diag/runner.log missing the expected mask marker — the assertion above would be vacuous"
  );
  Ok(())
}

#[tokio::test]
async fn zero_unmasked_secret_in_all_four_durable_sinks() -> TestResult {
  let masker = Arc::new(Mutex::new(SecretMasker::new()));
  masker
    .lock()
    .map_err(|e| format!("masker lock poisoned: {e}"))?
    .add_secret(SECRET);

  // Sink 4's writer must be installed before the job runs so the job's own
  // tracing output lands in the same file (see module doc).
  let tmp_home = init_file_sink(Arc::clone(&masker))?;

  let jobs_dir = tempfile::tempdir()?;
  let events = run_and_collect(Arc::clone(&masker), jobs_dir.path()).await?;

  assert_journal_clean(jobs_dir.path())?;
  assert_upload_sinks_clean(&events, &masker).await?;
  assert_runner_log_clean(&tmp_home)?;

  Ok(())
}
