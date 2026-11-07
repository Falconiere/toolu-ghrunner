//! Focused regression for a renewal or spool failure.

use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn step_upload_survives_combined_spool_creation_failure() -> TestResult<()> {
  let temp = tempfile::tempdir()?;
  let server_dir = temp.path().join("received");
  std::fs::create_dir(&server_dir)?;
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
  let base = format!("http://{}", listener.local_addr()?);
  let server = tokio::spawn(
    axum::serve(
      listener,
      Router::new()
        .fallback(any(receive))
        .with_state(ServerState {
          base: base.clone(),
          dir: server_dir.clone(),
        }),
    )
    .into_future(),
  );
  let (msg, step_id) = script_message("printf 'step-spool-probe-240\\n'")?;
  let config = shared::RunnerConfig {
    data_dir: temp.path().join("data"),
    workspace_root: temp.path().join("work"),
    workspace_gc_hours: 0,
    ..shared::RunnerConfig::default()
  };
  let logs = config.data_dir.join("_logs");
  std::fs::create_dir_all(&logs)?;
  let obstruction = logs.join(format!(
    "{}.log",
    shared::paths::sanitize_job_id(&msg.job_id)
  ));
  std::fs::create_dir(&obstruction)?;
  assert!(crate::log_spool::LogSpool::for_job(&config.data_dir, &msg.job_id).is_err());
  let masker = Arc::new(Mutex::new(SecretMasker::new()));
  let (tx, mut rx) = mpsc::channel(32);
  let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
  let ctx = SessionCtx {
    runner_name: None,
    client: reqwest::Client::new(),
    token: String::new(),
    broker_url: String::new(),
    session_id: String::new(),
    config: config.clone(),
    masker: Arc::clone(&masker),
    cancel: CancellationToken::new(),
    tx,
    encryption_key: None,
    use_fips_encryption: false,
    rsa_private_key_der: Vec::new(),
    refresh_auth: None,
    live_log: None,
    job_log_upload: None,
    watchdog: crate::helpers::WatchdogConfig::default(),
    job_acquired: false,
  };
  let cfg = FwdConfig {
    setup_cancel: CancellationToken::new(),
    data_dir: config.data_dir.clone(),
    setup_id: None,
    results_url: Some(base),
    results_client: ctx.client.clone(),
    results_token: "token".into(),
    run_backend_id: "run".into(),
    job_backend_id: "job".into(),
    setup_lines: Vec::new(),
    live_log_tx: None,
    masker,
  };
  let outcome = tokio::time::timeout(
    Duration::from_secs(30),
    run_forwarded_job(
      &ctx,
      &msg,
      &StepCollector::new(),
      cfg,
      &CancellationToken::new(),
    ),
  )
  .await?;
  assert_eq!(outcome.conclusion, Conclusion::Success);
  if let Some(upload) = outcome.job_log_upload {
    upload.await?;
  }
  drop(ctx);
  drain.await?;
  let mut decoded = String::new();
  let step_blob = server_dir.join(format!("step-{step_id}"));
  let file = std::fs::File::open(&step_blob)
    .map_err(|error| format!("step upload missing at {}: {error}", step_blob.display()))?;
  flate2::read::GzDecoder::new(file).read_to_string(&mut decoded)?;
  assert!(decoded.contains("step-spool-probe-240\n"));
  std::fs::remove_dir(obstruction)?;
  assert_no_spools(&logs)?;
  server.abort();
  Ok(())
}
