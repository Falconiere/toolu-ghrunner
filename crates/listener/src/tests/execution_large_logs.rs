//! Real runner and forwarder with disk-backed Results Service uploads.

use super::*;
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::{Router, body::Body, extract::State, http::Request, response::Response, routing::any};
use futures_util::StreamExt;

const SECRET: &str = "stream-secret-240";
const MIB: usize = 1024 * 1024;
type TestResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Clone)]
struct ServerState {
  base: String,
  dir: PathBuf,
}

async fn receive(State(state): State<ServerState>, request: Request<Body>) -> Response<Body> {
  let path = request.uri().path().to_owned();
  if path.starts_with("/step/") || path == "/job" {
    let filename = path.trim_start_matches('/').replace('/', "-");
    let Ok(mut file) = std::fs::File::create(state.dir.join(filename)) else {
      return Response::builder()
        .status(500)
        .body(Body::empty())
        .unwrap_or_default();
    };
    let mut body = request.into_body().into_data_stream();
    while let Some(chunk) = body.next().await {
      match chunk {
        Ok(bytes) if file.write_all(&bytes).is_ok() => {},
        _ => {
          return Response::builder()
            .status(500)
            .body(Body::empty())
            .unwrap_or_default();
        },
      }
    }
    return Response::new(Body::empty());
  }
  let blob = if path.ends_with("GetStepLogsSignedBlobURL") {
    let Ok(bytes) = axum::body::to_bytes(request.into_body(), 65536).await else {
      return Response::builder()
        .status(400)
        .body(Body::empty())
        .unwrap_or_default();
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
      return Response::builder()
        .status(400)
        .body(Body::empty())
        .unwrap_or_default();
    };
    let Some(id) = value
      .get("step_backend_id")
      .and_then(serde_json::Value::as_str)
    else {
      return Response::builder()
        .status(400)
        .body(Body::empty())
        .unwrap_or_default();
    };
    Some(format!("step/{id}"))
  } else if path.ends_with("GetJobLogsSignedBlobURL") {
    Some("job".to_owned())
  } else {
    None
  };
  let body = blob.map_or_else(String::new, |name| {
    serde_json::json!({"logs_url":format!("{}/{name}", state.base),"blob_storage_type":"BLOB_STORAGE_TYPE_AZURE"}).to_string()
  });
  Response::new(Body::from(body))
}

fn make_input(path: &Path) -> TestResult<()> {
  let mut out = std::io::BufWriter::new(std::fs::File::create(path)?);
  let block = vec![b'x'; MIB];
  for _ in 0..10 {
    out.write_all(&block)?;
  }
  writeln!(out, "{SECRET}")?;
  let mut seed = 0x1234_5678_u32;
  for _ in 0..=(195 * MIB / 65536) {
    let mut line = SECRET.to_owned();
    for _ in 0..65536 {
      seed ^= seed << 13;
      seed ^= seed >> 17;
      seed ^= seed << 5;
      line.push(char::from(b'!' + u8::try_from(seed % 90)?));
    }
    writeln!(out, "{line}")?;
  }
  out.flush()?;
  assert!(std::fs::metadata(path)?.len() >= 200 * MIB as u64);
  Ok(())
}

fn assert_gzip_matches(path: &Path, expected: &Path) -> TestResult<()> {
  let mut actual = BufReader::new(flate2::read::GzDecoder::new(std::fs::File::open(path)?));
  let mut expected = BufReader::new(std::fs::File::open(expected)?);
  let mut left = vec![0; 65536];
  let mut right = vec![0; 65536];
  loop {
    let a = actual.read(&mut left)?;
    if a == 0 {
      assert_eq!(expected.read(&mut right)?, 0);
      break;
    }
    let left_part = left.get(..a).ok_or("actual read length")?;
    let right_part = right.get_mut(..a).ok_or("expected read length")?;
    expected.read_exact(right_part)?;
    assert_eq!(left_part, right_part);
  }
  Ok(())
}

fn assert_no_spools(dir: &Path) -> TestResult<()> {
  if !dir.exists() {
    return Ok(());
  }
  for entry in std::fs::read_dir(dir)? {
    let path = entry?.path();
    if path.is_dir() {
      assert_no_spools(&path)?;
    } else {
      let name = path.file_name().ok_or("file name")?.to_string_lossy();
      assert!(
        !name.ends_with(".log") && !name.ends_with(".gz"),
        "spool remains: {path:?}"
      );
    }
  }
  Ok(())
}

fn rss_bytes() -> TestResult<u64> {
  let output = std::process::Command::new("ps")
    .args(["-o", "rss=", "-p", &std::process::id().to_string()])
    .output()?;
  if !output.status.success() {
    return Err("ps failed".into());
  }
  Ok(std::str::from_utf8(&output.stdout)?.trim().parse::<u64>()? * 1024)
}

fn script_message(script: &str) -> TestResult<(AgentJobRequestMessage, String)> {
  let mut raw: serde_json::Value = serde_json::from_str(include_str!(
    "../../../execution/tests/incoming_contexts_matrix_0.json"
  ))?;
  let steps = raw
    .get_mut("steps")
    .and_then(serde_json::Value::as_array_mut)
    .ok_or("steps")?;
  let mut step = steps
    .iter()
    .find(|s| {
      s.get("reference")
        .and_then(|r| r.get("type"))
        .and_then(serde_json::Value::as_str)
        == Some("script")
    })
    .ok_or("script step")?
    .clone();
  let step_id = step
    .get("id")
    .and_then(serde_json::Value::as_str)
    .ok_or("step id")?
    .to_owned();
  *step.get_mut("inputs").ok_or("inputs")? = serde_json::json!({"type":2,"map":[{"Key":{"type":0,"lit":"script"},"Value":{"type":0,"lit":script}}]});
  *step.get_mut("condition").ok_or("condition")? = serde_json::json!("success()");
  *steps = vec![step];
  let mut msg: AgentJobRequestMessage = serde_json::from_value(raw)?;
  msg.defaults.clear();
  msg.job_outputs = None;
  Ok((msg, step_id))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_child_large_logs_remain_bounded_and_upload_in_order() -> TestResult<()> {
  let temp = tempfile::tempdir()?;
  let input = temp.path().join("input");
  make_input(&input)?;
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

  let script = format!("cat '{}'", input.display());
  let (msg, step_id) = script_message(&script)?;
  let config = shared::RunnerConfig {
    data_dir: temp.path().join("data"),
    workspace_root: temp.path().join("work"),
    workspace_gc_hours: 0,
    ..shared::RunnerConfig::default()
  };
  let masker = Arc::new(Mutex::new(SecretMasker::new()));
  masker
    .lock()
    .map_err(|error| format!("masker: {error}"))?
    .add_secret(SECRET);
  let baseline = rss_bytes()?;
  let runner = Runner::new(config.clone(), Arc::clone(&masker));
  let cancel = CancellationToken::new();
  let cfg = FwdConfig {
    setup_cancel: cancel,
    data_dir: config.data_dir.clone(),
    results_url: Some(base),
    results_client: reqwest::Client::new(),
    results_token: "token".into(),
    run_backend_id: "run".into(),
    job_backend_id: "job".into(),
    setup_lines: Vec::new(),
    setup_id: None,
    live_log_tx: None,
    masker: Arc::clone(&masker),
  };
  let job_spool = crate::log_spool::LogSpool::for_job(&config.data_dir, &msg.job_id)?;
  let spool_path = config.data_dir.join("_logs").join(format!(
    "{}.log",
    shared::paths::sanitize_job_id(&msg.job_id)
  ));
  let (tx, mut rx) = mpsc::channel(8);
  let (outcome_tx, outcome_rx) = oneshot::channel();
  let expected_job = temp.path().join("expected-job");
  let expected_step = temp.path().join("expected-step");
  let job_file = expected_job.clone();
  let step_file = expected_step.clone();
  let captured_step_id = step_id.clone();
  let capture = tokio::spawn(async move {
    let mut job = std::io::BufWriter::new(std::fs::File::create(job_file)?);
    let mut step = std::io::BufWriter::new(std::fs::File::create(step_file)?);
    while let Some(event) = rx.recv().await {
      if let ListenerEvent::Runner(RunnerEvent::Log {
        step_id: id, line, ..
      }) = event
      {
        let line = line.replace(SECRET, "***");
        writeln!(job, "{line}")?;
        if id == captured_step_id {
          writeln!(step, "{line}")?;
        }
      }
    }
    job.flush()?;
    step.flush()?;
    Ok::<(), std::io::Error>(())
  });
  let (stop_tx, mut stop_rx) = tokio::sync::watch::channel(false);
  let sampler = tokio::spawn(async move {
    let mut peak = baseline;
    loop {
      peak = peak.max(rss_bytes()?);
      tokio::select! {
        _ = stop_rx.changed() => break,
        () = tokio::time::sleep(Duration::from_millis(25)) => {},
      }
    }
    Ok::<u64, Box<dyn std::error::Error + Send + Sync>>(peak)
  });
  let events = runner.execute_job_with_shutdown(
    msg.clone(),
    cfg.setup_cancel.clone(),
    cfg.setup_cancel.clone(),
  );
  let handle = spawn_event_forwarder(
    events,
    StepCollector::new(),
    tx,
    cfg,
    outcome_tx,
    Some(job_spool),
  );
  let outcome = tokio::time::timeout(Duration::from_secs(300), outcome_rx).await??;
  assert_eq!(outcome.conclusion, Conclusion::Success);
  if let Some(upload) = outcome.job_log_upload {
    upload.await?;
  } else {
    return Err("missing combined upload".into());
  }
  handle.await?;
  capture.await??;
  stop_tx.send(true)?;
  let peak = sampler.await??;
  assert!(
    peak.saturating_sub(baseline) < 128 * MIB as u64,
    "RSS grew {} bytes",
    peak.saturating_sub(baseline)
  );
  assert!(std::fs::metadata(&expected_step)?.len() >= 200 * MIB as u64);
  assert_gzip_matches(&server_dir.join(format!("step-{step_id}")), &expected_step)?;
  assert_gzip_matches(&server_dir.join("job"), &expected_job)?;
  assert!(!spool_path.exists(), "combined spool was not removed");
  let mut gzip_name = spool_path.as_os_str().to_os_string();
  gzip_name.push(".gz");
  assert!(
    !PathBuf::from(gzip_name).exists(),
    "compressed spool was not removed"
  );
  assert_no_spools(&config.data_dir.join("_logs"))?;
  server.abort();
  Ok(())
}

#[cfg(unix)]
#[test]
fn spool_is_private_and_replaces_stale_files() -> TestResult<()> {
  use std::os::unix::fs::PermissionsExt;

  let temp = tempfile::tempdir()?;
  let first = crate::log_spool::LogSpool::for_job(temp.path(), "stale-job")?;
  let path = temp.path().join("_logs/stale-job.log");
  let dir = path.parent().ok_or("spool directory")?.to_owned();
  assert_eq!(std::fs::metadata(&dir)?.permissions().mode() & 0o777, 0o700);
  assert_eq!(
    std::fs::metadata(&path)?.permissions().mode() & 0o777,
    0o600
  );
  drop(first);
  std::fs::write(&path, b"stale")?;
  let mut gz_name = path.as_os_str().to_os_string();
  gz_name.push(".gz");
  let gz_path = PathBuf::from(gz_name);
  std::fs::write(&gz_path, b"stale gzip")?;
  let _fresh = crate::log_spool::LogSpool::for_job(temp.path(), "stale-job")?;
  assert_eq!(std::fs::metadata(&path)?.len(), 0);
  assert!(!gz_path.exists());
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelled_real_child_uploads_logs_and_removes_spools() -> TestResult<()> {
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
  let (msg, step_id) = script_message("while :; do printf 'cancel-probe-240\\n'; done")?;
  let config = shared::RunnerConfig {
    data_dir: temp.path().join("data"),
    workspace_root: temp.path().join("work"),
    workspace_gc_hours: 0,
    ..shared::RunnerConfig::default()
  };
  let masker = Arc::new(Mutex::new(SecretMasker::new()));
  let runner = Runner::new(config.clone(), Arc::clone(&masker));
  let cancel = CancellationToken::new();
  let cfg = FwdConfig {
    setup_cancel: cancel.clone(),
    data_dir: config.data_dir.clone(),
    results_url: Some(base),
    results_client: reqwest::Client::new(),
    results_token: "token".into(),
    run_backend_id: "run".into(),
    job_backend_id: "job".into(),
    setup_lines: Vec::new(),
    setup_id: None,
    live_log_tx: None,
    masker,
  };
  let spool = crate::log_spool::LogSpool::for_job(&config.data_dir, &msg.job_id)?;
  let (tx, mut rx) = mpsc::channel(8);
  let (outcome_tx, outcome_rx) = oneshot::channel();
  let observer_cancel = cancel.clone();
  let observer = tokio::spawn(async move {
    let mut seen = 0;
    while let Some(event) = rx.recv().await {
      if matches!(event, ListenerEvent::Runner(RunnerEvent::Log { ref line, .. }) if line == "cancel-probe-240")
      {
        seen += 1;
        if seen == 16 {
          observer_cancel.cancel();
        }
      }
    }
    seen
  });
  let events = runner.execute_job_with_shutdown(msg, cancel, CancellationToken::new());
  let handle = spawn_event_forwarder(
    events,
    StepCollector::new(),
    tx,
    cfg,
    outcome_tx,
    Some(spool),
  );
  let outcome = tokio::time::timeout(Duration::from_secs(30), outcome_rx).await??;
  assert_eq!(outcome.conclusion, Conclusion::Cancelled);
  let upload = outcome.job_log_upload.ok_or("missing combined upload")?;
  tokio::time::timeout(Duration::from_secs(30), upload).await??;
  handle.await?;
  assert!(
    observer.await? >= 16,
    "child did not emit enough logs before cancellation"
  );
  for path in [
    server_dir.join(format!("step-{step_id}")),
    server_dir.join("job"),
  ] {
    let mut decoded = String::new();
    flate2::read::GzDecoder::new(std::fs::File::open(path)?).read_to_string(&mut decoded)?;
    assert!(decoded.contains("cancel-probe-240\n"));
  }
  assert_no_spools(&config.data_dir.join("_logs"))?;
  server.abort();
  Ok(())
}

#[path = "execution_large_logs_failure.rs"]
mod failure;
