//! Failure handling for Results Service step-summary uploads.

use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

async fn read_request(socket: &mut TcpStream) -> TestResult<(String, Vec<u8>)> {
  tokio::time::timeout(Duration::from_secs(5), async {
    let mut bytes = Vec::new();
    loop {
      let mut chunk = [0; 1024];
      let received = socket.read(&mut chunk).await?;
      if received == 0 {
        return Err("request ended before its complete body".into());
      }
      bytes.extend_from_slice(chunk.get(..received).ok_or("chunk range")?);
      if bytes.len() > 16_384 {
        return Err("request exceeds test limit".into());
      }
      if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
        let head = std::str::from_utf8(bytes.get(..end).ok_or("header range")?)?;
        let length = head
          .lines()
          .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key
              .eq_ignore_ascii_case("content-length")
              .then_some(value.trim())
          })
          .ok_or("missing content-length")?
          .parse::<usize>()?;
        if length > 8192 {
          return Err("body exceeds test limit".into());
        }
        let start = end + 4;
        if let Some(body) = bytes.get(start..start + length) {
          return Ok((head.to_owned(), body.to_vec()));
        }
      }
    }
  })
  .await?
}

fn assert_summary_rpc(head: &str, body: &[u8]) -> TestResult {
  assert_eq!(
    head.lines().next(),
    Some("POST /twirp/results.services.receiver.Receiver/GetStepSummarySignedBlobURL HTTP/1.1")
  );
  assert!(
    head
      .lines()
      .any(|line| line == "authorization: Bearer runtime-token")
  );
  assert_eq!(
    serde_json::from_slice::<serde_json::Value>(body)?,
    serde_json::json!({
      "workflow_run_backend_id": "workflow-run",
      "workflow_job_run_backend_id": "workflow-job",
      "step_backend_id": "step"
    })
  );
  Ok(())
}

async fn rpc_failure(status: &str, response_body: &str) -> TestResult<String> {
  let listener = TcpListener::bind("127.0.0.1:0").await?;
  let address = listener.local_addr()?;
  let reply = format!(
    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response_body}",
    response_body.len()
  );
  let server = tokio::spawn(async move {
    let (mut stream, _) = listener.accept().await?;
    let (head, body) = read_request(&mut stream).await?;
    assert_summary_rpc(&head, &body)?;
    stream.write_all(reply.as_bytes()).await?;
    Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
  });
  let result = super::upload_step_summary(
    &reqwest::Client::new(),
    &format!("http://{address}"),
    "runtime-token",
    &super::StepSummary {
      run_backend_id: "workflow-run",
      job_backend_id: "workflow-job",
      step_backend_id: "step",
      content: b"summary-secret",
    },
  )
  .await;
  tokio::time::timeout(Duration::from_secs(5), server).await???;
  Ok(result.unwrap_err().to_string())
}

/// A Results Service failure never exposes summary content or a signed URL.
#[tokio::test]
async fn step_summary_rpc_failure_is_safe() -> TestResult {
  let error = rpc_failure(
    "500 Internal Server Error",
    "summary-secret https://blob.example.invalid/summary?sig=secret-signature",
  )
  .await?;
  assert_eq!(
    error,
    "reporting error: GetStepSummarySignedBlobURL failed with HTTP 500"
  );
  assert!(!error.contains("summary-secret"));
  assert!(!error.contains("secret-signature"));
  assert!(!error.contains("runtime-token"));
  Ok(())
}

#[tokio::test]
async fn step_summary_malformed_reply_never_exposes_values() -> TestResult {
  let error = rpc_failure("200 OK", r#"{"summary_url":"https://blob.example.invalid/summary?sig=secret-signature","blob_storage_type":"BLOB_STORAGE_TYPE_AZURE","soft_size_limit":"summary-secret"}"#).await?;
  assert_eq!(
    error,
    "reporting error: GetStepSummarySignedBlobURL failed: invalid response"
  );
  assert!(!error.contains("summary-secret"));
  assert!(!error.contains("secret-signature"));
  Ok(())
}

#[tokio::test]
async fn step_summary_blob_failure_retries_three_times_without_leaking_url() -> TestResult {
  let server = TcpListener::bind("127.0.0.1:0").await?;
  let address = server.local_addr()?;
  let requests = tokio::spawn(async move {
    for _ in 0..4 {
      let (mut socket, _) = server.accept().await?;
      let (head, body) = read_request(&mut socket).await?;
      assert_eq!(
        head.lines().next(),
        Some("PUT /summary?sig=private HTTP/1.1")
      );
      assert!(head.lines().any(|line| line == "x-ms-blob-type: BlockBlob"));
      assert!(head.lines().any(|line| line == "content-type: text/plain"));
      assert_eq!(body, b"# summary\n");
      socket.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nRetry-After: 0\r\nConnection: close\r\n\r\n").await?;
    }
    Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
  });
  let response = super::SignedSummary {
    summary_url: format!("http://{address}/summary?sig=private"),
    blob_storage_type: "BLOB_STORAGE_TYPE_AZURE".to_owned(),
    soft_size_limit: 1_048_576,
  };
  let error = super::put_blob(&reqwest::Client::new(), &response, b"# summary\n")
    .await
    .unwrap_err()
    .to_string();
  tokio::time::timeout(Duration::from_secs(5), requests).await???;
  assert_eq!(
    error,
    "reporting error: summary blob PUT failed with HTTP 503"
  );
  assert!(!error.contains("private"));
  assert!(!error.contains(&response.summary_url));
  Ok(())
}

/// The masked document the success cases upload: `***` is what the engine's
/// masker leaves behind, so these are the exact bytes production would send.
const MASKED: &[u8] = b"# Step summary\n\nthe token is *** and stays masked\n";

/// Serve one complete three-call upload against a loopback peer, recording
/// every request. `limit` is the `soft_size_limit` the signed-URL RPC
/// advertises.
async fn success_peer(
  limit: u64,
  calls: usize,
) -> TestResult<(
  String,
  tokio::task::JoinHandle<TestResult<Vec<(String, Vec<u8>)>>>,
)> {
  let listener = TcpListener::bind("127.0.0.1:0").await?;
  let address = listener.local_addr()?;
  let base = format!("http://{address}");
  let signed = format!("{base}/blob/summary?sig=private");
  let server = tokio::spawn(async move {
    let mut seen = Vec::new();
    for _ in 0..calls {
      let (mut socket, _) = listener.accept().await?;
      let (head, body) = read_request(&mut socket).await?;
      let start = head.lines().next().unwrap_or_default().to_owned();
      let reply = if start.contains("GetStepSummarySignedBlobURL") {
        serde_json::json!({
          "summary_url": signed,
          "blob_storage_type": "BLOB_STORAGE_TYPE_AZURE",
          "soft_size_limit": limit.to_string(),
        })
        .to_string()
      } else if start.starts_with("PUT ") {
        String::new()
      } else {
        serde_json::json!({"ok": true}).to_string()
      };
      socket
        .write_all(
          format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{reply}",
            reply.len()
          )
          .as_bytes(),
        )
        .await?;
      socket.flush().await?;
      seen.push((head, body));
    }
    Ok::<_, Box<dyn std::error::Error + Send + Sync>>(seen)
  });
  Ok((base, server))
}

fn summary<'a>(content: &'a [u8]) -> super::StepSummary<'a> {
  super::StepSummary {
    run_backend_id: "workflow-run",
    job_backend_id: "workflow-job",
    step_backend_id: "step",
    content,
  }
}

/// #83 AC-1: the successful upload is exactly the signed-URL RPC, the blob
/// PUT of the masked bytes, and the metadata RPC — in that order, each
/// carrying the three backend identities and the real byte count.
#[tokio::test]
async fn step_summary_success_signs_puts_and_publishes_metadata() -> TestResult {
  let (base, server) = success_peer(1_048_576, 3).await?;
  super::upload_step_summary(
    &reqwest::Client::new(),
    &base,
    "runtime-token",
    &summary(MASKED),
  )
  .await?;
  let seen = tokio::time::timeout(Duration::from_secs(5), server).await???;
  let [signing, put, metadata] = seen.as_slice() else {
    return Err(format!("expected three calls, saw {}", seen.len()).into());
  };

  assert_summary_rpc(&signing.0, &signing.1)?;

  assert_eq!(
    put.0.lines().next(),
    Some("PUT /blob/summary?sig=private HTTP/1.1")
  );
  assert!(put.0.lines().any(|line| line == "content-type: text/plain"));
  assert!(
    put
      .0
      .lines()
      .any(|line| line == "x-ms-blob-type: BlockBlob")
  );
  assert_eq!(put.1, MASKED, "the blob is not the masked document");

  assert_eq!(
    metadata.0.lines().next(),
    Some("POST /twirp/results.services.receiver.Receiver/CreateStepSummaryMetadata HTTP/1.1")
  );
  assert!(
    metadata
      .0
      .lines()
      .any(|line| line == "authorization: Bearer runtime-token")
  );
  let published: serde_json::Value = serde_json::from_slice(&metadata.1)?;
  assert_eq!(
    published.get("workflow_run_backend_id"),
    Some(&serde_json::json!("workflow-run"))
  );
  assert_eq!(
    published.get("workflow_job_run_backend_id"),
    Some(&serde_json::json!("workflow-job"))
  );
  assert_eq!(
    published.get("step_backend_id"),
    Some(&serde_json::json!("step"))
  );
  assert_eq!(
    published.get("size"),
    Some(&serde_json::json!(MASKED.len())),
    "metadata size must be the uploaded byte count"
  );
  assert!(
    published
      .get("uploaded_at")
      .and_then(serde_json::Value::as_str)
      .is_some_and(|at| at.contains('T')),
    "metadata carries no RFC3339 timestamp: {published}"
  );
  Ok(())
}

/// #83 AC-1 boundary: a document exactly at the server's soft size limit is
/// uploaded; one byte over is refused before anything is written.
#[tokio::test]
async fn step_summary_soft_size_limit_is_inclusive_and_fails_before_the_put() -> TestResult {
  let limit = u64::try_from(MASKED.len())?;
  let (base, server) = success_peer(limit, 3).await?;
  super::upload_step_summary(
    &reqwest::Client::new(),
    &base,
    "runtime-token",
    &summary(MASKED),
  )
  .await?;
  assert_eq!(
    tokio::time::timeout(Duration::from_secs(5), server)
      .await???
      .len(),
    3,
    "a document at the limit must still upload"
  );

  let (base, server) = success_peer(limit - 1, 1).await?;
  let error = super::upload_step_summary(
    &reqwest::Client::new(),
    &base,
    "runtime-token",
    &summary(MASKED),
  )
  .await
  .unwrap_err()
  .to_string();
  assert_eq!(
    error,
    "reporting error: summary exceeds Results Service soft size limit"
  );
  assert_eq!(
    tokio::time::timeout(Duration::from_secs(5), server)
      .await???
      .len(),
    1,
    "an oversized document must never reach the blob PUT"
  );
  Ok(())
}
