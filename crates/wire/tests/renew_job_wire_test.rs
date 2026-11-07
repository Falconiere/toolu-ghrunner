//! Renewal response classification against a loopback HTTP server.

use shared::RunnerError;
use tokio::net::TcpListener;
use wire::reporting::run_service::{RenewJobRequest, RenewJobResponse, renew_job};

use crate::toolu_jobs_wire_test::{response_bytes, serve_once};

async fn renew(status: u16, body: &str) -> Result<Option<RenewJobResponse>, RunnerError> {
  let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
  let url = format!("http://{}", listener.local_addr().expect("address"));
  let server = tokio::spawn(serve_once(listener, response_bytes(status, &[], body)));
  let request = RenewJobRequest {
    plan_id: "plan".to_owned(),
    job_id: "job".to_owned(),
  };
  let result = renew_job(&reqwest::Client::new(), &url, "token", &request).await;
  let received = server.await.expect("server");
  assert_eq!(received.request_line, "POST /renewjob HTTP/1.1");
  result
}

#[tokio::test]
async fn missing_job_returns_none() {
  let result = renew(404, "not found").await;
  assert!(matches!(result, Ok(None)), "{result:?}");
}

#[tokio::test]
async fn unauthorized_remains_a_protocol_error() {
  let result = renew(401, "unauthorized").await;
  assert!(
    matches!(result, Err(RunnerError::Protocol(_))),
    "{result:?}"
  );
}

#[tokio::test]
async fn unavailable_remains_a_network_error() {
  let result = renew(503, "unavailable").await;
  assert!(matches!(result, Err(RunnerError::Network(_))), "{result:?}");
}

#[tokio::test]
async fn malformed_success_remains_a_protocol_error() {
  let result = renew(200, "not json").await;
  assert!(
    matches!(result, Err(RunnerError::Protocol(_))),
    "{result:?}"
  );
}
