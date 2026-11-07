//! Preserve the first renewal failure through a real delayed log upload.

use super::*;

#[tokio::test]
async fn timed_out_reason_survives_404_during_teardown() -> TestResult<()> {
  let dir = tempfile::tempdir()?;
  let server = MockServer::start().await;
  let uri = server.uri();
  let mut msg = job_message(&uri, "job-sticky-reason", "printf 'probe\\n'; sleep 30");
  msg.variables.insert(
    "system.github.results_endpoint".to_owned(),
    shared::VariableValue {
      value: uri.clone(),
      is_secret: false,
    },
  );
  mount_message_poll(&server, "rr-sticky", &uri).await;
  mount_acquire_job(&server, "plan-1", &msg).await?;
  mount_acknowledge(&server, 200).await;
  Mock::given(method("POST"))
    .and(path("/renewjob"))
    .respond_with(ResponseTemplate::new(503))
    .up_to_n_times(5)
    .mount(&server)
    .await;
  mount_renew_job_always(&server, 404).await;
  mount_complete_job(&server, 0).await;
  for method_name in ["GetStepLogsSignedBlobURL", "GetJobLogsSignedBlobURL"] {
    Mock::given(method("POST"))
      .and(path(format!(
        "/twirp/results.services.receiver.Receiver/{method_name}"
      )))
      .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
        "logs_url": format!("{uri}/blob"), "blob_storage_type": "BLOB_STORAGE_TYPE_AZURE"
      })))
      .mount(&server)
      .await;
  }
  Mock::given(method("PUT"))
    .and(path("/blob"))
    .respond_with(ResponseTemplate::new(201).set_delay(Duration::from_secs(2)))
    .mount(&server)
    .await;
  for method_name in ["CreateStepLogsMetadata", "CreateJobLogsMetadata"] {
    Mock::given(method("POST"))
      .and(path(format!(
        "/twirp/results.services.receiver.Receiver/{method_name}"
      )))
      .respond_with(ResponseTemplate::new(200))
      .mount(&server)
      .await;
  }
  Mock::given(method("POST"))
    .and(path(
      "/twirp/github.actions.results.api.v1.WorkflowStepUpdateService/WorkflowStepsUpdate",
    ))
    .respond_with(ResponseTemplate::new(200))
    .mount(&server)
    .await;
  let watchdog = WatchdogConfig {
    outage_threshold: Duration::from_millis(200),
    renew_interval: Duration::from_millis(100),
  };
  let mut ctx = make_ctx(uri, make_config(&dir), watchdog)?;
  let result = tokio::time::timeout(
    Duration::from_secs(20),
    poll_and_execute(&mut ctx, &CancellationToken::new()),
  )
  .await??;
  assert_eq!(result, Some(Conclusion::Failure));
  let requests = server
    .received_requests()
    .await
    .ok_or("request recording disabled")?;
  let renewals: Vec<_> = requests
    .iter()
    .enumerate()
    .filter(|(_, r)| r.url.path() == "/renewjob")
    .collect();
  assert!(
    renewals.len() >= 6,
    "expected five 503s followed by 404, got {} renewals",
    renewals.len()
  );
  let sixth = renewals.get(5).ok_or("missing 404 renewal")?.0;
  let blob = requests
    .iter()
    .position(|r| r.url.path() == "/blob")
    .ok_or("missing delayed blob upload")?;
  assert!(
    blob < sixth,
    "404 must arrive while the delayed upload is active"
  );
  let complete: Vec<_> = requests
    .iter()
    .filter(|r| r.url.path() == "/completejob")
    .collect();
  assert_request_count(
    &complete.iter().map(|r| (*r).clone()).collect::<Vec<_>>(),
    1,
    "completejob",
  )?;
  let body = complete
    .first()
    .ok_or("missing completejob")?
    .body_json::<serde_json::Value>()?;
  assert_failure_with_lost_connection(&body)
}
