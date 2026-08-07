//! Tests for `execution_loop`: `forward_log_line` / `handle_event_arm`
//! masking guarantees.

use super::*;

/// Fresh masker with one registered secret, matching the shape of the
/// real `ExecutionContext::register_secret` runtime path.
fn masker_with_secret(secret: &str) -> Arc<Mutex<SecretMasker>> {
  let masker = Arc::new(Mutex::new(SecretMasker::new()));
  masker
    .lock()
    .expect("fresh mutex is never poisoned")
    .add_secret(secret);
  masker
}

/// Minimal `FwdConfig` for driving `forward_log_line` directly. Only
/// `masker` (and, per test, `live_log_tx`) matter to this fn — the
/// Results Service fields are never read by it.
fn test_fwd_config(masker: Arc<Mutex<SecretMasker>>) -> FwdConfig {
  FwdConfig {
    results_url: None,
    results_client: reqwest::Client::new(),
    results_token: String::new(),
    run_backend_id: String::new(),
    job_backend_id: String::new(),
    setup_lines: Vec::new(),
    live_log_tx: None,
    masker,
  }
}

/// AC-5 production-path guard: calls the REAL `forward_log_line` — the
/// exact fn `handle_event_arm` invokes on every `RunnerEvent::Log` —
/// and proves a registered secret never reaches either sink it fans
/// out to: the combined job log (`state.all_job_lines`, sink 1) and the
/// per-step upload channel (`state.uploaders`, sink 2, the same channel
/// `spawn_step_uploader` would hand to the log streamer). Deleting the
/// `mask_line` call inside `forward_log_line` must fail this test —
/// verified by hand while writing it.
#[tokio::test]
async fn forward_log_line_masks_both_the_job_log_and_the_step_upload_channel() {
  const SECRET: &str = "s3cr3t-exec-loop-forward-4d9a1c";
  let masker = masker_with_secret(SECRET);
  let cfg = test_fwd_config(masker);
  let mut state = ForwarderState::new(Vec::new());

  let step_id = "step-1";
  let (tx, mut rx) = mpsc::channel::<String>(4);
  state.uploaders.insert(step_id.to_owned(), tx);

  let raw_line = format!("leaking {SECRET} now");
  forward_log_line(&mut state, &cfg, step_id, &raw_line).await;

  let job_log_line = state
    .all_job_lines
    .last()
    .expect("forward_log_line should have pushed exactly one line");
  assert_eq!(
    job_log_line, "leaking *** now",
    "combined job log line must be masked exactly, not merely contain a marker"
  );

  let uploaded_line = rx
    .recv()
    .await
    .expect("forward_log_line should have sent one line to the step upload channel");
  assert_eq!(
    uploaded_line, "leaking *** now",
    "per-step upload line must be masked exactly, not merely contain a marker"
  );
}

/// A dispatcher-produced line is masked exactly like a child-stdout line.
///
/// `command_dispatch::log_event` builds a plain `RunnerEvent::Log` for the
/// echoed `::<cmd>::` line, and `handle_event_arm` matches
/// `RunnerEvent::Log { .. }` with no discrimination on who produced it — so
/// a synthetic line takes the same masked path. Pinned as a test because a
/// reviewer read the dispatcher's unmasked push as a leak, on the theory
/// that synthetic lines reach the sinks some other way. They do not: this
/// drives the whole arm, not just `forward_log_line`.
#[tokio::test]
async fn a_dispatcher_echoed_command_line_is_masked_like_any_other_log() {
  const SECRET: &str = "s3cr3t-echoed-command-9f2b71";
  let masker = masker_with_secret(SECRET);
  let cfg = test_fwd_config(masker);
  let mut state = ForwarderState::new(Vec::new());

  let step_id = "step-1";
  let (tx, mut rx) = mpsc::channel::<String>(4);
  state.uploaders.insert(step_id.to_owned(), tx);

  // Byte-for-byte the shape `command_dispatch::log_event` emits for an
  // echoed command line, routed through the real dispatch arm.
  let event = RunnerEvent::Log {
    step_id: step_id.to_owned(),
    line: format!("::set-output name=token::{SECRET}"),
    stream: shared::LogStream::Stdout,
  };
  handle_event_arm(&mut state, &cfg, &event).await;

  let job_log_line = state
    .all_job_lines
    .last()
    .expect("the Log arm should have pushed exactly one line");
  assert_eq!(
    job_log_line, "::set-output name=token::***",
    "echoed command line must be masked exactly, not merely contain a marker"
  );

  let uploaded_line = rx
    .recv()
    .await
    .expect("the Log arm should have sent one line to the step upload channel");
  assert_eq!(
    uploaded_line, "::set-output name=token::***",
    "per-step upload line must be masked exactly, not merely contain a marker"
  );
}
