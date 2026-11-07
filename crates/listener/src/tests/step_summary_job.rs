//! Issue #83: a real job's `$GITHUB_STEP_SUMMARY` document, carried by the
//! forwarder to a recording Results Service — masked, identified and
//! unable to change the job's result.
//!
//! Declared from `execution_loop.rs` (not beside the queue's own tests in
//! `tests/step_summary.rs`) because the engine + forwarder harness these
//! need lives in that module's scope; the file name keeps both under the
//! `step_summary` test filter.

use super::*;
use std::collections::HashMap;
use std::error::Error;

use axum::http::{Method, StatusCode};

use crate::support::{RecordingServer, RouteResponse};

type TestResult = Result<(), Box<dyn Error + Send + Sync>>;

/// Results Service receiver routes, as `wire::net::step_summary` builds them.
const SIGN: &str = "/twirp/results.services.receiver.Receiver/GetStepSummarySignedBlobURL";
const PUBLISH: &str = "/twirp/results.services.receiver.Receiver/CreateStepSummaryMetadata";
/// Where the peer's signed URL points.
const BLOB: &str = "/blob/summary";
/// Registered through `::add-mask::` by the job's first step, so the
/// document its second step writes can only reach the service masked.
const SUMMARY_SECRET: &str = "summary-probe-secret-83";

/// Routes for a peer that answers a complete upload, with `sign_status`
/// deciding whether the signed-URL RPC succeeds.
fn upload_routes(base: &str, sign_status: StatusCode) -> HashMap<(Method, String), RouteResponse> {
  HashMap::from([
    (
      (Method::POST, SIGN.to_owned()),
      RouteResponse {
        status: sign_status,
        body: serde_json::json!({
          "summary_url": format!("{base}{BLOB}"),
          "blob_storage_type": "BLOB_STORAGE_TYPE_AZURE",
          "soft_size_limit": "1048576",
        })
        .to_string()
        .into_bytes(),
        hold: None,
      },
    ),
    (
      (Method::PUT, BLOB.to_owned()),
      RouteResponse {
        status: StatusCode::CREATED,
        body: Vec::new(),
        hold: None,
      },
    ),
    (
      (Method::POST, PUBLISH.to_owned()),
      RouteResponse::ok_json(&serde_json::json!({"ok": true})),
    ),
  ])
}

/// A real two-step job: register a mask, then write a summary containing
/// the masked value.
fn summary_job() -> Result<AgentJobRequestMessage, Box<dyn Error + Send + Sync>> {
  let mut message: AgentJobRequestMessage = serde_json::from_str(include_str!(
    "../../../execution/tests/job_outputs_message.json"
  ))?;
  message.steps = vec![
    shared::ActionStep::script(
      "11111111-aaaa-4bbb-8ccc-000000000001",
      &format!("printf '%s\\n' '::add-mask::{SUMMARY_SECRET}'"),
      "",
    ),
    shared::ActionStep::script(
      "11111111-aaaa-4bbb-8ccc-000000000002",
      &format!("printf '# summary-83\\nvalue={SUMMARY_SECRET}\\n' >> \"$GITHUB_STEP_SUMMARY\""),
      "",
    ),
  ];
  Ok(message)
}

/// Run `summary_job` through the production engine and event forwarder,
/// reporting summaries to `peer`.
async fn run_against(peer: &RecordingServer) -> Result<Conclusion, Box<dyn Error + Send + Sync>> {
  let dir = tempfile::tempdir()?;
  let config = shared::RunnerConfig {
    data_dir: dir.path().join("data"),
    workspace_root: dir.path().join("work"),
    ..shared::RunnerConfig::default()
  };
  let masker = Arc::new(Mutex::new(SecretMasker::new()));
  let runner = Runner::new(config, Arc::clone(&masker));
  let engine_rx = runner.execute_job(summary_job()?, CancellationToken::new());
  let cfg = FwdConfig {
    setup_cancel: CancellationToken::new(),
    setup_id: None,
    results_url: Some(peer.base_url()),
    results_client: reqwest::Client::new(),
    results_token: "runtime-token".to_owned(),
    run_backend_id: "workflow-run".to_owned(),
    job_backend_id: "workflow-job".to_owned(),
    setup_lines: Vec::new(),
    live_log_tx: None,
    masker,
  };
  let (fwd_tx, mut fwd_rx) = mpsc::channel(128);
  let (outcome_tx, outcome_rx) = oneshot::channel();
  let handle = spawn_event_forwarder(engine_rx, StepCollector::new(), fwd_tx, cfg, outcome_tx);
  let outcome = tokio::time::timeout(std::time::Duration::from_secs(60), outcome_rx).await??;
  handle.await?;
  while fwd_rx.recv().await.is_some() {}
  Ok(outcome.conclusion)
}

/// #83 AC-1: a real step's summary reaches the Results Service through the
/// full three-call upload, carrying the job's backend identities and the
/// masked bytes — never the registered secret.
#[tokio::test]
async fn step_summary_real_job_document_is_uploaded_masked() -> TestResult {
  let peer = RecordingServer::start(|base| upload_routes(base, StatusCode::OK)).await;
  assert_eq!(run_against(&peer).await?, Conclusion::Success);

  let signing = peer.requests_for(SIGN);
  let [sign] = signing.as_slice() else {
    return Err(format!("expected one signed-URL call, saw {}", signing.len()).into());
  };
  assert_eq!(
    sign.authorization.as_deref(),
    Some("Bearer runtime-token"),
    "the Results Service call is unauthenticated"
  );
  let ids: serde_json::Value = serde_json::from_slice(&sign.body)?;
  assert_eq!(
    ids.get("workflow_run_backend_id"),
    Some(&serde_json::json!("workflow-run"))
  );
  assert_eq!(
    ids.get("workflow_job_run_backend_id"),
    Some(&serde_json::json!("workflow-job"))
  );
  let step_id = ids
    .get("step_backend_id")
    .and_then(serde_json::Value::as_str)
    .ok_or("the signed-URL call named no step")?
    .to_owned();

  let blobs = peer.requests_for(BLOB);
  let [blob] = blobs.as_slice() else {
    return Err(format!("expected one blob PUT, saw {}", blobs.len()).into());
  };
  let document = String::from_utf8(blob.body.clone())?;
  assert_eq!(
    document, "# summary-83\nvalue=***\n",
    "the uploaded document is not the masked one"
  );

  let published = peer.requests_for(PUBLISH);
  let [publish] = published.as_slice() else {
    return Err(format!("expected one metadata call, saw {}", published.len()).into());
  };
  let metadata: serde_json::Value = serde_json::from_slice(&publish.body)?;
  assert_eq!(
    metadata.get("step_backend_id"),
    Some(&serde_json::json!(step_id)),
    "metadata and signed URL disagree about the step"
  );
  assert_eq!(
    metadata.get("size"),
    Some(&serde_json::json!(blob.body.len())),
    "metadata size is not the uploaded byte count"
  );
  // The document is signed, written, then published.
  assert!(sign.arrived_at <= blob.arrived_at && blob.arrived_at <= publish.arrived_at);
  Ok(())
}

/// #83 AC-1: a Results Service that rejects the signed-URL call leaves the
/// job's conclusion alone and never reaches the blob PUT.
#[tokio::test]
async fn step_summary_upload_failure_never_changes_the_job_result() -> TestResult {
  let peer =
    RecordingServer::start(|base| upload_routes(base, StatusCode::SERVICE_UNAVAILABLE)).await;
  assert_eq!(
    run_against(&peer).await?,
    Conclusion::Success,
    "a rejected summary failed the job"
  );
  assert_eq!(peer.requests_for(SIGN).len(), 1);
  assert!(
    peer.requests_for(BLOB).is_empty(),
    "a rejected signing call must not write a blob"
  );
  Ok(())
}
