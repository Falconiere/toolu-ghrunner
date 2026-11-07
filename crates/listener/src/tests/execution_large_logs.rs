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
  if path == "/step" || path == "/job" {
    let mut file = match std::fs::File::create(state.dir.join(&path[1..])) {
      Ok(file) => file,
      Err(_) => {
        return Response::builder()
          .status(500)
          .body(Body::empty())
          .unwrap_or_default();
      },
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
    Some("step")
  } else if path.ends_with("GetJobLogsSignedBlobURL") {
    Some("job")
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
  let mut line = format!("{SECRET}");
  for _ in 0..65536 {
    seed ^= seed << 13;
    seed ^= seed >> 17;
    seed ^= seed << 5;
    line.push(char::from(b'!' + u8::try_from(seed % 90)?));
  }
  line.push('\n');
  for _ in 0..(195 * MIB / line.len() + 1) {
    out.write_all(line.as_bytes())?;
  }
  out.flush()?;
  assert!(std::fs::metadata(path)?.len() >= 200 * MIB as u64);
  Ok(())
}

fn assert_gzip_matches(path: &Path, expected: &Path) -> TestResult<()> {
  let mut actual = BufReader::new(flate2::read::GzDecoder::new(std::fs::File::open(path)?));
  let mut expected = BufReader::new(std::fs::File::open(expected)?);
  let mut left = [0; 65536];
  let mut right = [0; 65536];
  loop {
    let a = actual.read(&mut left)?;
    if a == 0 {
      assert_eq!(expected.read(&mut right)?, 0);
      break;
    }
    expected.read_exact(&mut right[..a])?;
    assert_eq!(&left[..a], &right[..a]);
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
        !name.ends_with(".log") && !name.ends_with(".log.gz"),
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

  let mut raw: serde_json::Value = serde_json::from_str(include_str!(
    "../../../execution/tests/incoming_contexts_matrix_0.json"
  ))?;
  let steps = raw
    .get_mut("steps")
    .and_then(serde_json::Value::as_array_mut)
    .ok_or("steps")?;
  let mut step = steps
    .iter()
    .find(|s| s["reference"]["type"] == "script")
    .ok_or("script step")?
    .clone();
  let step_id = step["id"].as_str().ok_or("step id")?.to_owned();
  let script = format!("cat '{}'", input.display());
  step["inputs"] = serde_json::json!({"type":2,"map":[{"Key":{"type":0,"lit":"script"},"Value":{"type":0,"lit":script}}]});
  step["condition"] = serde_json::json!("success()");
  *steps = vec![step];
  let mut msg: AgentJobRequestMessage = serde_json::from_value(raw)?;
  msg.defaults.clear();
  msg.job_outputs = None;
  let config = shared::RunnerConfig {
    data_dir: temp.path().join("data"),
    workspace_root: temp.path().join("work"),
    workspace_gc_hours: 0,
    ..shared::RunnerConfig::default()
  };
  let masker = Arc::new(Mutex::new(SecretMasker::new()));
  masker.lock().map_err(|_| "masker")?.add_secret(SECRET);
  let baseline = rss_bytes()?;
  let runner = Runner::new(config.clone(), Arc::clone(&masker));
  let cancel = CancellationToken::new();
  let cfg = FwdConfig {
    setup_cancel: cancel,
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
  let spool_path = job_spool.path().to_owned();
  let (tx, mut rx) = mpsc::channel(8);
  let (outcome_tx, outcome_rx) = oneshot::channel();
  let expected_job = temp.path().join("expected-job");
  let expected_step = temp.path().join("expected-step");
  let job_file = expected_job.clone();
  let step_file = expected_step.clone();
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
        if id == step_id {
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
  assert_gzip_matches(&server_dir.join("step"), &expected_step)?;
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
