//! Failure handling for Results Service step-summary uploads.

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// A Results Service failure never exposes summary content or a signed URL.
#[tokio::test]
async fn step_summary_rpc_failure_is_safe() -> Result<(), Box<dyn std::error::Error>> {
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let address = listener.local_addr().unwrap();
  let server = tokio::spawn(async move {
    let (mut stream, _) = listener.accept().await?;
    let mut request = [0_u8; 4096];
    let received = stream.read(&mut request).await?;
    assert!(received > 0, "client must send a summary RPC request");
    stream
      .write_all(
        b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 70\r\nConnection: close\r\n\r\nsummary-secret https://blob.example.invalid/summary?sig=secret-signature",
      )
      .await?;
    Ok::<_, std::io::Error>(())
  });

  let error = super::upload_step_summary(
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
  .await
  .unwrap_err()
  .to_string();

  server.await??;
  assert!(error.contains("GetStepSummarySignedBlobURL"));
  assert!(!error.contains("summary-secret"));
  assert!(!error.contains("secret-signature"));
  assert!(!error.contains("runtime-token"));
  Ok(())
}

#[tokio::test]
async fn step_summary_blob_failure_retries_three_times_without_leaking_url() {
  let server = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let address = server.local_addr().unwrap();
  let requests = tokio::spawn(async move {
    for _ in 0..4 {
      let (mut socket, _) = server.accept().await.unwrap();
      let mut bytes = [0; 8192];
      let size = socket.read(&mut bytes).await.unwrap();
      let text = String::from_utf8_lossy(bytes.get(..size).unwrap());
      assert!(text.starts_with("PUT /summary?sig=private "));
      assert!(text.contains("x-ms-blob-type: BlockBlob"));
      socket.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nRetry-After: 0\r\nConnection: close\r\n\r\n").await.unwrap();
    }
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
  assert!(error.contains("503"));
  assert!(!error.contains("private"));
  requests.await.unwrap();
}
