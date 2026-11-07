//! Issue #83: the summary upload queue's own behaviour — saturation past
//! its slot count and a transport failure that must not strand later
//! documents. The job-level upload path lives in `step_summary_job.rs`.
use super::*;
use std::collections::HashMap;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use axum::http::{Method, StatusCode};

use crate::support::{Gate, RecordingServer, RouteResponse};

/// Results Service receiver routes, as `wire::net::step_summary` builds them.
const SIGN: &str = "/twirp/results.services.receiver.Receiver/GetStepSummarySignedBlobURL";
const PUBLISH: &str = "/twirp/results.services.receiver.Receiver/CreateStepSummaryMetadata";
/// Where the peer's signed URL points.
const BLOB: &str = "/blob/summary";
/// Routes for a peer that answers a complete upload, with `hold` gating the
/// signed-URL reply so the test can stall it.
fn upload_routes(base: &str, hold: Gate) -> HashMap<(Method, String), RouteResponse> {
  let mut routes = HashMap::new();
  routes.insert(
    (Method::POST, SIGN.to_owned()),
    RouteResponse {
      status: StatusCode::OK,
      body: serde_json::json!({
        "summary_url": format!("{base}{BLOB}"),
        "blob_storage_type": "BLOB_STORAGE_TYPE_AZURE",
        "soft_size_limit": "1048576",
      })
      .to_string()
      .into_bytes(),
      hold: Some(hold),
    },
  );
  routes.insert(
    (Method::PUT, BLOB.to_owned()),
    RouteResponse {
      status: StatusCode::CREATED,
      body: Vec::new(),
      hold: None,
    },
  );
  routes.insert(
    (Method::POST, PUBLISH.to_owned()),
    RouteResponse {
      status: StatusCode::OK,
      body: serde_json::json!({"ok": true}).to_string().into_bytes(),
      hold: None,
    },
  );
  routes
}

/// #83 AC-1: the queue buffers 16 documents and the uploader holds one more
/// in flight, so the eighteenth applies backpressure rather than being
/// dropped, and the drain still uploads every one.
///
/// The peer is stalled while the queue fills, which is also the ceiling a
/// stalled Results Service can impose on job completion: uploads are serial
/// and every document costs at most one 30s `wire` request deadline, so 18
/// held documents can delay completion by about 9 minutes (about 38 minutes
/// if the stall is in the blob PUT: four 30s attempts plus 5.6s of backoff
/// per document, and up to about 90 minutes when `Retry-After` replaces the
/// backoff). Recorded in docs/step-summaries.md; the test releases the stall
/// instead of waiting it out.
#[tokio::test]
async fn step_summary_queue_drains_more_documents_than_its_slots()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
  let gate = Gate::new();
  let held = gate.clone();
  let peer = RecordingServer::start(move |base| upload_routes(base, held)).await;
  let queue = StepSummaryQueue::spawn(StepReportQueueConfig {
    client: reqwest::Client::new(),
    results_url: peer.base_url(),
    token: "runtime-token".to_owned(),
    run_backend_id: "workflow-run".to_owned(),
    job_backend_id: "workflow-job".to_owned(),
  });
  let queue = std::sync::Arc::new(queue);
  let filler = tokio::spawn({
    let queue = std::sync::Arc::clone(&queue);
    async move {
      for index in 0..18 {
        queue
          .enqueue(&format!("step-{index}"), format!("document {index}\n"))
          .await;
      }
    }
  });
  // With the peer held, the uploader cannot drain: 16 documents fit the
  // channel and a seventeenth is in flight, so the eighteenth must still be
  // waiting for a slot. An unbounded queue would have finished here.
  tokio::time::sleep(std::time::Duration::from_millis(250)).await;
  assert!(
    !filler.is_finished(),
    "the queue accepted 18 documents while no upload could complete"
  );
  gate.release();
  filler.await?;
  let queue = std::sync::Arc::into_inner(queue).ok_or("the filler task still holds the queue")?;
  tokio::time::timeout(std::time::Duration::from_secs(60), queue.drain()).await?;
  assert_eq!(
    peer.requests_for(SIGN).len(),
    18,
    "the drain dropped documents"
  );
  assert_eq!(peer.requests_for(BLOB).len(), 18);
  assert_eq!(peer.requests_for(PUBLISH).len(), 18);
  Ok(())
}

#[tokio::test]
async fn step_summary_failure_drains_later_documents() -> Result<(), Box<dyn std::error::Error>> {
  let server = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
  let address = server.local_addr()?;
  let requests = tokio::spawn(async move {
    let mut seen = Vec::new();
    for _ in 0..2 {
      let (mut socket, _) = server.accept().await?;
      let mut bytes = vec![0; 8192];
      let size = socket.read(&mut bytes).await?;
      seen.push(String::from_utf8_lossy(bytes.get(..size).ok_or("request range")?).into_owned());
      socket
        .write_all(
          b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        )
        .await?;
    }
    Ok::<_, Box<dyn std::error::Error + Send + Sync>>(seen)
  });
  let queue = StepSummaryQueue::spawn(StepReportQueueConfig {
    client: reqwest::Client::new(),
    results_url: format!("http://{address}"),
    token: String::new(),
    run_backend_id: "run".to_owned(),
    job_backend_id: "job".to_owned(),
  });
  queue.enqueue("first", "first document".to_owned()).await;
  queue.enqueue("second", "second document".to_owned()).await;
  tokio::time::timeout(std::time::Duration::from_secs(5), queue.drain()).await?;
  let seen = tokio::time::timeout(std::time::Duration::from_secs(5), requests)
    .await??
    .map_err(|error| error.to_string())?;
  assert_eq!(seen.len(), 2);
  assert!(seen.first().ok_or("first request")?.contains("first"));
  assert!(seen.last().ok_or("second request")?.contains("second"));
  Ok(())
}
