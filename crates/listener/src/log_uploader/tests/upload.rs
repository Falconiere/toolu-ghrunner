//! Disk-backed uploads against loopback Results Service and blob endpoints.

use std::io::{Read, Write};
use std::time::Duration;

use flate2::read::GzDecoder;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::{upload_job_logs, upload_step_logs};
use crate::helpers::ResultsCtx;

type TestResult = Result<(), Box<dyn std::error::Error>>;
const LINES: &[u8] = b"first ***\nsecond line\n";
const RECEIVER: &str = "/twirp/results.services.receiver.Receiver/";

fn assert_upload_requests(
  requests: &[wiremock::Request],
  step: bool,
  metadata: &str,
) -> TestResult {
  let puts: Vec<_> = requests
    .iter()
    .filter(|r| r.url.path() == "/blob")
    .collect();
  assert_eq!(puts.len(), 2, "retry after the first PUT fails");
  for put in puts {
    assert_eq!(
      put.headers.get("content-type").ok_or("missing type")?,
      "application/gzip"
    );
    assert_eq!(
      put
        .headers
        .get("content-encoding")
        .ok_or("missing encoding")?,
      "gzip"
    );
    assert_eq!(
      put
        .headers
        .get("x-ms-blob-type")
        .ok_or("missing blob type")?,
      "BlockBlob"
    );
    let length: usize = put
      .headers
      .get("content-length")
      .ok_or("missing length")?
      .to_str()?
      .parse()?;
    assert_eq!(length, put.body.len());
    let mut decoded = Vec::new();
    GzDecoder::new(put.body.as_slice()).read_to_end(&mut decoded)?;
    assert_eq!(decoded, LINES);
  }
  let meta: Vec<_> = requests
    .iter()
    .filter(|r| r.url.path() == format!("{RECEIVER}{metadata}"))
    .collect();
  assert_eq!(meta.len(), 1);
  let body: serde_json::Value = meta.first().ok_or("missing metadata")?.body_json()?;
  assert_eq!(body.get("line_count"), Some(&serde_json::json!(2)));
  if step {
    assert_eq!(
      body.get("step_backend_id"),
      Some(&serde_json::json!("step"))
    );
  }
  Ok(())
}

async fn exercise_upload(step: bool) -> TestResult {
  let dir = tempfile::tempdir()?;
  let mut file = tempfile::NamedTempFile::new_in(dir.path())?;
  file.write_all(LINES)?;
  let (_, spool) = file.keep()?;
  let server = MockServer::start().await;
  let uri = server.uri();
  let (get, metadata) = if step {
    ("GetStepLogsSignedBlobURL", "CreateStepLogsMetadata")
  } else {
    ("GetJobLogsSignedBlobURL", "CreateJobLogsMetadata")
  };
  Mock::given(method("POST"))
    .and(path(format!("{RECEIVER}{get}")))
    .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
      "logs_url": format!("{uri}/blob"),
      "blob_storage_type": "BLOB_STORAGE_TYPE_AZURE"
    })))
    .mount(&server)
    .await;
  Mock::given(method("PUT"))
    .and(path("/blob"))
    .respond_with(ResponseTemplate::new(500))
    .up_to_n_times(1)
    .mount(&server)
    .await;
  Mock::given(method("PUT"))
    .and(path("/blob"))
    .respond_with(ResponseTemplate::new(201))
    .mount(&server)
    .await;
  Mock::given(method("POST"))
    .and(path(format!("{RECEIVER}{metadata}")))
    .respond_with(ResponseTemplate::new(200))
    .mount(&server)
    .await;

  let client = reqwest::Client::new();
  let rctx = ResultsCtx {
    client: &client,
    results_url: &uri,
    token: "token",
    run_backend_id: "run",
    job_backend_id: "job",
  };
  if step {
    assert_eq!(
      upload_step_logs(&rctx, "step", spool, 2).await,
      Some((format!("{uri}/blob"), 2))
    );
  } else {
    assert_eq!(upload_job_logs(&rctx, spool, 2).await, Some(2));
  }

  let requests = server.received_requests().await.ok_or("no requests")?;
  assert_upload_requests(&requests, step, metadata)?;
  assert_eq!(
    std::fs::read_dir(dir.path())?.count(),
    0,
    "spool and gzip files are removed"
  );
  Ok(())
}

#[tokio::test]
async fn job_upload_retries_and_cleans_up() -> TestResult {
  exercise_upload(false).await
}

#[tokio::test]
async fn step_upload_retries_and_cleans_up() -> TestResult {
  exercise_upload(true).await
}

#[tokio::test]
async fn failed_step_upload_removes_both_files() -> TestResult {
  let dir = tempfile::tempdir()?;
  let spool = dir.path().join("step.log");
  std::fs::write(&spool, LINES)?;
  let server = MockServer::start().await;
  let uri = server.uri();
  Mock::given(method("POST"))
    .and(path(format!("{RECEIVER}GetStepLogsSignedBlobURL")))
    .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
      "logs_url": format!("{uri}/blob"), "blob_storage_type": "BLOB_STORAGE_TYPE_AZURE"
    })))
    .mount(&server)
    .await;
  Mock::given(method("PUT"))
    .and(path("/blob"))
    .respond_with(ResponseTemplate::new(500))
    .mount(&server)
    .await;
  let client = reqwest::Client::new();
  let rctx = ResultsCtx {
    client: &client,
    results_url: &uri,
    token: "token",
    run_backend_id: "run",
    job_backend_id: "job",
  };
  assert_eq!(upload_step_logs(&rctx, "step", spool, 2).await, None);
  assert_eq!(std::fs::read_dir(dir.path())?.count(), 0);
  Ok(())
}

#[tokio::test]
async fn aborted_job_upload_removes_both_files() -> TestResult {
  let dir = tempfile::tempdir()?;
  let spool = dir.path().join("job.log");
  std::fs::write(&spool, LINES)?;
  let server = MockServer::start().await;
  let uri = server.uri();
  Mock::given(method("POST"))
    .and(path(format!("{RECEIVER}GetJobLogsSignedBlobURL")))
    .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
      "logs_url": format!("{uri}/blob"), "blob_storage_type": "BLOB_STORAGE_TYPE_AZURE"
    })))
    .mount(&server)
    .await;
  Mock::given(method("PUT"))
    .and(path("/blob"))
    .respond_with(ResponseTemplate::new(201).set_delay(Duration::from_secs(10)))
    .mount(&server)
    .await;
  let task = tokio::spawn(async move {
    let client = reqwest::Client::new();
    let rctx = ResultsCtx {
      client: &client,
      results_url: &uri,
      token: "token",
      run_backend_id: "run",
      job_backend_id: "job",
    };
    upload_job_logs(&rctx, spool, 2).await
  });
  tokio::time::timeout(Duration::from_secs(5), async {
    loop {
      if server
        .received_requests()
        .await
        .is_some_and(|requests| requests.iter().any(|r| r.url.path() == "/blob"))
      {
        break;
      }
      tokio::time::sleep(Duration::from_millis(10)).await;
    }
  })
  .await?;
  task.abort();
  let _ = task.await;
  assert_eq!(std::fs::read_dir(dir.path())?.count(), 0);
  Ok(())
}
