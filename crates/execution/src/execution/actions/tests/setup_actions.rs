//! Pinned public action archive through production extraction/cache/logging.
//! Static HTTP endpoints replay unmodified GitHub REST and codeload captures.

use super::super::resolver::parse_action_ref;
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
const ARCHIVE: &[u8] = include_bytes!("../../../../tests/setup_action.tar.gz");
const REVISION: &str = include_str!("../../../../tests/setup_action_commit.json");
const SHA: &str = "8bcd8e1af3c095561f1043123848fc8b2db0f189";

async fn archive_server() -> TestResult<(String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>)> {
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
  let url = format!("http://{}", listener.local_addr()?);
  let requests = Arc::new(AtomicUsize::new(0));
  let count = Arc::clone(&requests);
  let router = axum::Router::new().route(
    &format!("/repos/actions/hello-world-docker-action/tarball/{SHA}"),
    axum::routing::get(move || {
      count.fetch_add(1, Ordering::SeqCst);
      async { ARCHIVE }
    }),
  );
  let router = router.route(
    "/repos/actions/hello-world-docker-action/commits/main",
    axum::routing::get(|| async { REVISION }),
  );
  let handle = tokio::spawn(async move {
    axum::serve(listener, router)
      .await
      .expect("static archive server");
  });
  Ok((url, requests, handle))
}

fn captured_job(api: &str) -> TestResult<AgentJobRequestMessage> {
  let mut raw: serde_json::Value = serde_json::from_str(include_str!(
    "../../../../tests/incoming_contexts_matrix_0.json"
  ))?;
  let github = raw
    .get_mut("contextData")
    .and_then(|v| v.get_mut("github"))
    .and_then(|v| v.get_mut("d"))
    .and_then(serde_json::Value::as_array_mut)
    .ok_or("github context")?;
  for entry in github {
    if entry.get("k").and_then(serde_json::Value::as_str) == Some("api_url") {
      *entry.get_mut("v").ok_or("api_url value")? = api.into();
    }
  }
  let mut msg: AgentJobRequestMessage = serde_json::from_value(raw)?;
  msg.variables.remove("system.github.launch_endpoint");
  Ok(msg)
}

fn fetcher(
  msg: &AgentJobRequestMessage,
  events: mpsc::Sender<shared::RunnerEvent>,
) -> ActionFetcher {
  ActionFetcher::for_job(
    msg,
    Arc::new(Mutex::new(SecretMasker::new())),
    CancellationToken::new(),
  )
  .with_events(events)
}

#[tokio::test]
async fn setup_action_real_archive_cold_warm_dedup_and_subpath() -> TestResult {
  let (url, requests, server) = archive_server().await?;
  let msg = captured_job(&url)?;
  let temp = tempfile::tempdir()?;
  let (tx, mut rx) = mpsc::channel(16);
  let fetcher = fetcher(&msg, tx);
  let client = reqwest::Client::new();
  let action = parse_action_ref("actions/hello-world-docker-action@main")?;
  let path = fetcher.ensure_action(&client, &action, temp.path()).await?;
  assert!(std::fs::read_to_string(path.join("action.yml"))?.contains("using: docker"));
  fetcher.ensure_action(&client, &action, temp.path()).await?;
  assert_eq!(requests.load(Ordering::SeqCst), 1);
  let subpath = parse_action_ref("actions/hello-world-docker-action/.github@main")?;
  // Existing real subdirectory exercises ref rendering, not action execution.
  fetcher
    .ensure_action(&client, &subpath, temp.path())
    .await?;
  drop(fetcher);
  let mut lines = Vec::new();
  while let Some(event) = rx.recv().await {
    let shared::RunnerEvent::Log { step_id, line, .. } = event else {
      return Err("unexpected event".into());
    };
    assert_eq!(step_id, shared::SETUP_STEP_ID);
    assert!(!line.contains(&url));
    lines.push(line);
  }
  assert_eq!(
    lines,
    vec![
      format!("Prepare action 'actions/hello-world-docker-action@main' (SHA:{SHA})"),
      format!("Prepare action 'actions/hello-world-docker-action/.github@main' (SHA:{SHA})"),
    ]
  );
  let (tx, mut rx) = mpsc::channel(4);
  let warm = self::fetcher(&msg, tx);
  warm.ensure_action(&client, &action, temp.path()).await?;
  assert_eq!(
    requests.load(Ordering::SeqCst),
    1,
    "new job uses disk cache"
  );
  drop(warm);
  assert!(
    matches!(rx.recv().await, Some(shared::RunnerEvent::Log { line, .. }) if line == lines.first().cloned().unwrap_or_default())
  );
  assert!(rx.recv().await.is_none());
  server.abort();
  Ok(())
}

#[tokio::test]
async fn setup_action_failed_download_has_no_success_identity() -> TestResult {
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
  let url = format!("http://{}/unavailable", listener.local_addr()?);
  drop(listener);
  let msg = captured_job(&url)?;
  let action = parse_action_ref("actions/hello-world-docker-action@main")?;
  let temp = tempfile::tempdir()?;
  let (tx, mut rx) = mpsc::channel(4);
  let fetcher = fetcher(&msg, tx);
  assert!(
    fetcher
      .ensure_action(&reqwest::Client::new(), &action, temp.path())
      .await
      .is_err()
  );
  drop(fetcher);
  assert!(rx.recv().await.is_none());
  Ok(())
}

#[tokio::test]
async fn setup_action_production_prefetch_emits_resolved_sha() -> TestResult {
  let (api, requests, server) = archive_server().await?;
  let mut msg = captured_job(&api)?;
  let temp = tempfile::tempdir()?;
  let config = shared::RunnerConfig {
    data_dir: temp.path().join("data"),
    workspace_root: temp.path().join("work"),
    workspace_gc_hours: 0,
    ..shared::RunnerConfig::default()
  };
  let hash = blake3::hash(api.as_bytes()).to_hex();
  let prefix = hash.get(..16).ok_or("host hash")?;
  let marker = config.data_dir.join(format!(
    "actions/{prefix}/actions/hello-world-docker-action/{SHA}/.completed"
  ));
  msg.variables.insert(
    "SETUP_CACHE_MARKER".to_owned(),
    shared::VariableValue {
      value: marker.to_string_lossy().into_owned(),
      is_secret: false,
    },
  );
  let mut remote = msg.steps.first().ok_or("captured checkout")?.clone();
  remote.reference.name = Some("actions/hello-world-docker-action".to_owned());
  remote.reference.git_ref = Some("main".to_owned());
  remote.condition = Some("false".to_owned());
  // Prefetch runs for top-level refs even when execution is skipped. A real shell
  // observes the extraction marker; no Docker process or response is simulated.
  let mut script = msg.steps.get(1).ok_or("captured script")?.clone();
  script.inputs = shared::ActionStep::script(
    "unused",
    r#"
i=0
while [ "$i" -lt 100 ]; do
  if test -f "$SETUP_CACHE_MARKER"; then echo archive-ready; exit 0; fi
  sleep 0.1
  i=$((i+1))
done
exit 1
"#,
    "",
  )
  .inputs;
  script.condition = Some("success()".to_owned());
  msg.steps = vec![remote, script];
  msg.defaults.clear();
  msg.job_outputs = None;
  let runner = crate::Runner::new(config, Arc::new(Mutex::new(SecretMasker::new())));
  let mut rx = runner.execute_job(msg, CancellationToken::new());
  let mut diagnostics = Vec::new();
  let mut conclusion = None;
  let mut marker_observed = false;
  tokio::time::timeout(std::time::Duration::from_secs(20), async {
    while let Some(event) = rx.recv().await {
      match event {
        shared::RunnerEvent::Log { step_id, line, .. } if step_id == shared::SETUP_STEP_ID => {
          diagnostics.push(line);
        },
        shared::RunnerEvent::Log { line, .. } if line == "archive-ready" => marker_observed = true,
        shared::RunnerEvent::JobCompleted {
          conclusion: result, ..
        } => conclusion = Some(result),
        shared::RunnerEvent::JobStarted { .. }
        | shared::RunnerEvent::StepStarted { .. }
        | shared::RunnerEvent::StepCompleted { .. }
        | shared::RunnerEvent::StepSkipped { .. }
        | shared::RunnerEvent::Log { .. }
        | shared::RunnerEvent::StepSummary { .. }
        | shared::RunnerEvent::LogGroup { .. }
        | shared::RunnerEvent::Annotation { .. } => {},
      }
    }
  })
  .await?;
  assert_eq!(conclusion, Some(shared::Conclusion::Success));
  assert!(marker_observed);
  assert_eq!(
    diagnostics,
    vec![format!(
      "Prepare action 'actions/hello-world-docker-action@main' (SHA:{SHA})"
    )]
  );
  assert_eq!(requests.load(Ordering::SeqCst), 1);
  server.abort();
  Ok(())
}
