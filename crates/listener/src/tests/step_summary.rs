//! Queue transport failure regression; successful service parity is a live lane.
use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

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
