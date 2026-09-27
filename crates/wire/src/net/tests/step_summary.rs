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
