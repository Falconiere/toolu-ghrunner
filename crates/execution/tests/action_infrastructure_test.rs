//! Issue 88: runner-side action fetch failures carry upstream's
//! `infrastructureFailureCategory` (`resolve_action`, `error_download_action`,
//! `invalid_action_download`); user errors carry none.
//!
//! The job is the sanitized github.com acquisition of `completejob-88.yml`'s
//! `steps-toolu` job (run 38098611227) with its captured `actions/checkout@v4`
//! step. Its launch endpoint is pointed at local HTTP servers standing in for
//! GitHub. Most serve **injected faults** — the statuses and cut streams under
//! test; the #235 success cases serve a real Launch resolution and a real
//! archive instead, so the resolved URL, the scoped token and the extracted
//! action are observed rather than assumed. Archives are real gzip tarballs
//! built here (or deliberately corrupt bytes).

use std::collections::HashMap;
use std::error::Error;
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use execution::Runner;
use shared::{
  AgentJobRequestMessage, Conclusion, RunnerConfig, RunnerEvent, SecretMasker, TemplateToken,
  VariableValue,
};
use tokio_util::sync::CancellationToken;

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

const CAPTURE: &str = include_str!("completejob_88_steps_message.json");
const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
/// The only file in the served archive; the action the job then runs.
const MANIFEST: &[u8] =
  b"name: probe\nruns:\n  using: composite\n  steps:\n    - shell: bash\n      run: echo fetched\n";
/// Synthetic scoped archive credential the Launch peer hands back. It
/// authenticates nothing; the tests watch where it travels and who masks it.
const ARCHIVE_TOKEN: &str = "launch-scoped-archive-probe-235";

/// What the fault-injection archive endpoint serves on each request.
#[derive(Clone, Copy)]
enum Archive {
  Status(StatusCode),
  /// The real tarball on every request.
  Ok,
  /// 401 on the first request, a valid action afterwards.
  UnauthorizedOnce,
  /// A valid gzip header, then the connection drops mid-body.
  CutMidBody,
  Corrupt,
}

/// What the launch download-info endpoint answers.
#[derive(Clone, Copy)]
enum Launch {
  Status(StatusCode),
  Resolve(Archive),
}

fn action_tarball() -> TestResult<Vec<u8>> {
  let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
    Vec::new(),
    flate2::Compression::default(),
  ));
  let mut header = tar::Header::new_gnu();
  header.set_size(u64::try_from(MANIFEST.len())?);
  header.set_mode(0o644);
  header.set_cksum();
  builder.append_data(&mut header, "actions-checkout-0123456/action.yml", MANIFEST)?;
  Ok(builder.into_inner()?.finish()?)
}

/// One request the fault-injection peer answered, in arrival order.
#[derive(Clone, Debug)]
struct Recorded {
  path: String,
  authorization: Option<String>,
  body: String,
}

/// A running fault-injection peer and every request it has answered.
struct Peer {
  base: String,
  seen: Arc<Mutex<Vec<Recorded>>>,
}

impl Peer {
  /// The recorded requests for one path, in arrival order.
  fn seen(&self, path: &str) -> Vec<Recorded> {
    self
      .seen
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .iter()
      .filter(|request| request.path == path)
      .cloned()
      .collect()
  }
}

fn record(
  seen: &Mutex<Vec<Recorded>>,
  uri: &axum::http::Uri,
  headers: &axum::http::HeaderMap,
  body: String,
) {
  seen
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .push(Recorded {
      path: uri.path().to_owned(),
      authorization: headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned),
      body,
    });
}

async fn serve(launch: Launch) -> TestResult<Peer> {
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
  let base = format!("http://{}", listener.local_addr()?);
  let tarball = action_tarball()?;
  let archive_hits = Arc::new(AtomicUsize::new(0));
  let seen: Arc<Mutex<Vec<Recorded>>> = Arc::new(Mutex::new(Vec::new()));
  let tar_url = format!("{base}/archive");
  let launch_seen = Arc::clone(&seen);
  let launch_route = move |uri: axum::http::Uri, headers: axum::http::HeaderMap, body: String| {
    let tar_url = tar_url.clone();
    let seen = Arc::clone(&launch_seen);
    async move {
      record(&seen, &uri, &headers, body);
      match launch {
        Launch::Status(code) => code.into_response(),
        Launch::Resolve(_) => axum::Json(serde_json::json!({"actions": {
          "actions/checkout@v4": {
            "resolved_name": "actions/checkout",
            "resolved_sha": SHA,
            "tar_url": tar_url,
            "authentication": {"token": ARCHIVE_TOKEN},
          }
        }}))
        .into_response(),
      }
    }
  };
  let archive_seen = Arc::clone(&seen);
  let archive_route = move |uri: axum::http::Uri, headers: axum::http::HeaderMap| {
    let hits = Arc::clone(&archive_hits);
    let tarball = tarball.clone();
    let seen = Arc::clone(&archive_seen);
    async move {
      record(&seen, &uri, &headers, String::new());
      let first = hits.fetch_add(1, Ordering::SeqCst) == 0;
      let Launch::Resolve(archive) = launch else {
        return StatusCode::NOT_FOUND.into_response();
      };
      match archive {
        Archive::Status(code) => code.into_response(),
        Archive::UnauthorizedOnce if first => StatusCode::UNAUTHORIZED.into_response(),
        Archive::Ok | Archive::UnauthorizedOnce => tarball.into_response(),
        Archive::Corrupt => b"this is not a gzip archive".to_vec().into_response(),
        Archive::CutMidBody => {
          let head = tarball.get(..10).unwrap_or_default().to_vec();
          let chunks: Vec<Result<Vec<u8>, std::io::Error>> = vec![
            Ok(head),
            Err(std::io::Error::other("injected connection reset")),
          ];
          Response::new(Body::from_stream(futures_util::stream::iter(chunks)))
        },
      }
    }
  };
  let app = axum::Router::new()
    .route("/archive", axum::routing::get(archive_route))
    .fallback(launch_route);
  tokio::spawn(async move {
    if let Err(error) = axum::serve(listener, app).await {
      eprintln!("fault-injection server stopped: {error}");
    }
  });
  Ok(Peer { base, seen })
}

/// The captured job's own Launch route and `SystemVssConnection` credential,
/// read from the capture exactly as production reads them.
fn launch_path_and_token() -> TestResult<(String, String)> {
  let raw: serde_json::Value = serde_json::from_str(CAPTURE)?;
  let plan = raw
    .pointer("/plan/planId")
    .and_then(serde_json::Value::as_str)
    .ok_or("captured plan id missing")?;
  let job = raw
    .pointer("/jobId")
    .and_then(serde_json::Value::as_str)
    .ok_or("captured job id missing")?;
  let endpoint = raw
    .pointer("/resources/endpoints/0")
    .ok_or("captured service endpoint missing")?;
  if endpoint.get("name").and_then(serde_json::Value::as_str) != Some("SystemVssConnection") {
    return Err("first captured endpoint is not SystemVssConnection".into());
  }
  let token = endpoint
    .pointer("/authorization/parameters/AccessToken")
    .and_then(serde_json::Value::as_str)
    .ok_or("captured service token missing")?;
  Ok((
    format!("/actions/build/{plan}/jobs/{job}/runnerresolve/actions"),
    token.to_owned(),
  ))
}

/// The `action.yml` of every extracted action under a job's `data_dir`.
fn extracted_manifests(
  data_dir: &std::path::Path,
) -> TestResult<Vec<(std::path::PathBuf, Vec<u8>)>> {
  let mut found = Vec::new();
  let mut stack = vec![data_dir.join("actions")];
  while let Some(dir) = stack.pop() {
    let Ok(entries) = std::fs::read_dir(&dir) else {
      continue;
    };
    for entry in entries {
      let path = entry?.path();
      if path.is_dir() {
        stack.push(path);
      } else if path.file_name().is_some_and(|name| name == "action.yml") {
        let bytes = std::fs::read(&path)?;
        found.push((path, bytes));
      }
    }
  }
  Ok(found)
}

/// The captured job, reduced to its checkout step and one later bash step,
/// with its launch endpoint pointed at `base`.
fn job(base: &str) -> TestResult<AgentJobRequestMessage> {
  let mut message: AgentJobRequestMessage = serde_json::from_str(CAPTURE)?;
  message.steps.truncate(2);
  message.variables.insert(
    "system.github.launch_endpoint".to_owned(),
    VariableValue {
      value: base.to_owned(),
      ..message
        .variables
        .get("system.github.launch_endpoint")
        .cloned()
        .ok_or("captured launch endpoint missing")?
    },
  );
  Ok(message)
}

#[derive(Debug)]
struct Observed {
  conclusion: Option<Conclusion>,
  /// `(step id, category, message)` of every infrastructure error.
  infrastructure: Vec<(String, String, String)>,
  steps: HashMap<String, Conclusion>,
  /// Every emitted log line, in arrival order.
  logs: Vec<String>,
}

async fn run(message: AgentJobRequestMessage) -> TestResult<Observed> {
  Ok(run_observed(message).await?.2)
}

/// `run`, keeping the job's `data_dir` alive and handing back its masker so a
/// caller can inspect the extracted cache and what the masker learned.
async fn run_observed(
  message: AgentJobRequestMessage,
) -> TestResult<(tempfile::TempDir, Arc<Mutex<SecretMasker>>, Observed)> {
  let dir = tempfile::tempdir()?;
  let config = RunnerConfig {
    data_dir: dir.path().join("data"),
    workspace_root: dir.path().join("work"),
    workspace_gc_hours: 0,
    workspace: shared::WorkspaceConfig {
      mode: shared::WorkspaceMode::Fresh,
      stable_gc_hours: 0,
    },
    ..RunnerConfig::default()
  };
  let workspace = config.workspace_root.join(&message.job_id);
  seed_nested_composite(&workspace)?;
  let masker = Arc::new(Mutex::new(SecretMasker::new()));
  let runner = Runner::new(config, Arc::clone(&masker));
  let mut stream = runner.execute_job(message, CancellationToken::new());
  let mut observed = Observed {
    conclusion: None,
    infrastructure: Vec::new(),
    steps: HashMap::new(),
    logs: Vec::new(),
  };
  tokio::time::timeout(Duration::from_secs(120), async {
    while let Some(event) = stream.recv().await {
      if let RunnerEvent::InfrastructureError {
        step_id,
        category,
        message,
      } = &event
      {
        observed
          .infrastructure
          .push((step_id.clone(), category.clone(), message.clone()));
      }
      if let RunnerEvent::StepCompleted {
        step_id,
        conclusion,
        ..
      } = &event
      {
        observed.steps.insert(step_id.clone(), *conclusion);
      }
      if let RunnerEvent::JobCompleted { conclusion, .. } = &event {
        observed.conclusion = Some(*conclusion);
      }
      if let RunnerEvent::Log { line, .. } = &event {
        observed.logs.push(line.clone());
      }
    }
  })
  .await?;
  Ok((dir, masker, observed))
}

/// A local composite action whose only child fetches the remote action.
fn seed_nested_composite(workspace: &std::path::Path) -> TestResult {
  let dir = workspace.join(".github/actions/nested-fetch");
  std::fs::create_dir_all(&dir)?;
  let mut file = std::fs::File::create(dir.join("action.yml"))?;
  file.write_all(
    b"name: nested-fetch\nruns:\n  using: composite\n  steps:\n    - uses: actions/checkout@v4\n",
  )?;
  Ok(())
}

async fn category_for(launch: Launch) -> TestResult<(Peer, Observed, String)> {
  let peer = serve(launch).await?;
  let message = job(&peer.base)?;
  let checkout = message.steps.first().ok_or("checkout missing")?.id.clone();
  let observed = run(message).await?;
  Ok((peer, observed, checkout))
}

#[tokio::test]
async fn runner_side_fetch_failures_carry_upstream_categories() -> TestResult {
  for (launch, category) in [
    (
      Launch::Status(StatusCode::INTERNAL_SERVER_ERROR),
      "resolve_action",
    ),
    (
      Launch::Status(StatusCode::TOO_MANY_REQUESTS),
      "resolve_action",
    ),
    (
      Launch::Resolve(Archive::Status(StatusCode::INTERNAL_SERVER_ERROR)),
      "error_download_action",
    ),
    (
      Launch::Resolve(Archive::Status(StatusCode::NOT_FOUND)),
      "error_download_action",
    ),
    (
      Launch::Resolve(Archive::Status(StatusCode::UNAUTHORIZED)),
      "error_download_action",
    ),
    (
      Launch::Resolve(Archive::CutMidBody),
      "error_download_action",
    ),
    (Launch::Resolve(Archive::Corrupt), "invalid_action_download"),
  ] {
    let (_peer, observed, checkout) = category_for(launch).await?;
    assert_eq!(observed.conclusion, Some(Conclusion::Failure), "{category}");
    assert_eq!(observed.steps.get(&checkout), Some(&Conclusion::Failure));
    let [(step, found, message)] = observed.infrastructure.as_slice() else {
      return Err(format!("{category}: {:?}", observed.infrastructure).into());
    };
    assert_eq!(
      (step.as_str(), found.as_str()),
      (checkout.as_str(), category)
    );
    assert!(!message.starts_with("action download failed"), "{message}");
  }
  Ok(())
}

#[tokio::test]
async fn user_fetch_failures_and_recovered_auth_carry_no_category() -> TestResult {
  for (launch, conclusion) in [
    // LaunchHttpClient: 422 is "unresolvable" (missing repo or ref).
    (
      Launch::Status(StatusCode::UNPROCESSABLE_ENTITY),
      Conclusion::Failure,
    ),
    // AccessDeniedException is the workflow's, not the runner's.
    (
      Launch::Resolve(Archive::Status(StatusCode::FORBIDDEN)),
      Conclusion::Failure,
    ),
    // One auth refresh recovers a 401; nothing failed.
    (
      Launch::Resolve(Archive::UnauthorizedOnce),
      Conclusion::Success,
    ),
  ] {
    let (_peer, observed, _) = category_for(launch).await?;
    assert_eq!(observed.conclusion, Some(conclusion));
    assert!(
      observed.infrastructure.is_empty(),
      "{:?}",
      observed.infrastructure
    );
  }
  Ok(())
}

#[tokio::test]
async fn continue_on_error_fetch_failure_still_reports_the_error() -> TestResult {
  let peer = serve(Launch::Status(StatusCode::INTERNAL_SERVER_ERROR)).await?;
  let mut message = job(&peer.base)?;
  let checkout = message.steps.first_mut().ok_or("checkout missing")?;
  checkout.continue_on_error = Some(TemplateToken {
    token_type: 5,
    bool_val: Some(true),
    ..TemplateToken::default()
  });
  let id = checkout.id.clone();
  let observed = run(message).await?;
  // The step turns green; the listener sets no job category for it.
  assert_eq!(observed.conclusion, Some(Conclusion::Success));
  assert_eq!(observed.steps.get(&id), Some(&Conclusion::Success));
  assert_eq!(observed.infrastructure.len(), 1);
  Ok(())
}

#[tokio::test]
async fn nested_composite_fetch_failure_is_attributed_to_the_enclosing_step() -> TestResult {
  let peer = serve(Launch::Resolve(Archive::Status(
    StatusCode::SERVICE_UNAVAILABLE,
  )))
  .await?;
  let mut message = job(&peer.base)?;
  // Turn the captured checkout step into the local composite that nests it.
  let step = message.steps.first_mut().ok_or("checkout missing")?;
  step.reference = serde_json::from_value(serde_json::json!({
    "type": "repository",
    "repositoryType": "self",
    "path": "./.github/actions/nested-fetch"
  }))?;
  let id = step.id.clone();
  let observed = run(message).await?;
  assert_eq!(observed.conclusion, Some(Conclusion::Failure));
  let [(step, category, _)] = observed.infrastructure.as_slice() else {
    return Err(format!("{:?}", observed.infrastructure).into());
  };
  assert_eq!(
    (step.as_str(), category.as_str()),
    (id.as_str(), "error_download_action")
  );
  Ok(())
}

/// #76 AC-1 / 76-S1: a successful Launch resolution is consumed end to end —
/// the server-selected URL and scoped token are used, the real archive is
/// extracted, and the action the archive carries actually runs.
#[tokio::test]
async fn launch_success_uses_the_served_url_token_and_runs_the_action() -> TestResult {
  let (launch_path, service_token) = launch_path_and_token()?;
  let peer = serve(Launch::Resolve(Archive::Ok)).await?;
  let message = job(&peer.base)?;
  let checkout = message.steps.first().ok_or("checkout missing")?.id.clone();
  let (dir, _masker, observed) = run_observed(message).await?;

  // Measured: the action is resolved twice — once by the setup prefetch and
  // once by the step that uses it. Both requests are identical and idempotent.
  let resolutions = peer.seen(&launch_path);
  assert_eq!(resolutions.len(), 2, "{resolutions:?}");
  for resolution in &resolutions {
    assert_eq!(
      resolution.authorization.as_deref(),
      Some(format!("Bearer {service_token}").as_str())
    );
    assert_eq!(
      serde_json::from_str::<serde_json::Value>(&resolution.body)?,
      serde_json::json!({"actions": [{"action": "actions/checkout", "version": "v4"}]})
    );
  }

  let basic = base64::Engine::encode(
    &base64::engine::general_purpose::STANDARD,
    format!("x-access-token:{ARCHIVE_TOKEN}"),
  );
  let archives = peer.seen("/archive");
  let [archive] = archives.as_slice() else {
    return Err(format!("expected one archive request, saw {archives:?}").into());
  };
  assert_eq!(
    archive.authorization.as_deref(),
    Some(format!("Basic {basic}").as_str()),
    "the scoped Launch token must authenticate the archive request"
  );

  assert_eq!(
    observed.conclusion,
    Some(Conclusion::Success),
    "{observed:?}"
  );
  assert_eq!(observed.steps.get(&checkout), Some(&Conclusion::Success));
  assert!(
    observed.infrastructure.is_empty(),
    "{:?}",
    observed.infrastructure
  );
  assert!(
    observed
      .logs
      .iter()
      .any(|line| line == &format!("Prepare action 'actions/checkout@v4' (SHA:{SHA})")),
    "{:?}",
    observed.logs
  );
  assert!(
    observed.logs.iter().any(|line| line == "fetched"),
    "the downloaded action never ran: {:?}",
    observed.logs
  );

  let extracted = extracted_manifests(&dir.path().join("data"))?;
  let [(path, bytes)] = extracted.as_slice() else {
    return Err(format!("expected one extracted action, saw {extracted:?}").into());
  };
  assert_eq!(bytes.as_slice(), MANIFEST, "extracted bytes differ");
  assert!(
    path.to_string_lossy().contains(SHA),
    "the cache entry is not keyed by the resolved revision: {path:?}"
  );
  Ok(())
}

/// #76 AC-4: the scoped archive credential reaches the job masker in both the
/// raw and the Basic-encoded form production registers, and neither form — nor
/// the archive URL — appears in any line the job emitted.
#[tokio::test]
async fn launch_scoped_token_is_masked_and_never_logged() -> TestResult {
  let peer = serve(Launch::Resolve(Archive::Ok)).await?;
  let archive_url = format!("{}/archive", peer.base);
  let message = job(&peer.base)?;
  let (_dir, masker, observed) = run_observed(message).await?;
  assert_eq!(observed.conclusion, Some(Conclusion::Success));

  let basic = base64::Engine::encode(
    &base64::engine::general_purpose::STANDARD,
    format!("x-access-token:{ARCHIVE_TOKEN}"),
  );
  let guard = masker
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner);
  assert_eq!(guard.mask(ARCHIVE_TOKEN), "***");
  assert_eq!(guard.mask(&basic), "***");
  assert_eq!(
    guard.mask(&format!("downloading with Basic {basic}")),
    "downloading with Basic ***"
  );
  drop(guard);

  for line in &observed.logs {
    assert!(!line.contains(ARCHIVE_TOKEN), "raw token logged: {line}");
    assert!(!line.contains(&basic), "Basic credential logged: {line}");
    assert!(!line.contains(&archive_url), "archive URL logged: {line}");
  }
  for (_, _, message) in &observed.infrastructure {
    assert!(
      !message.contains(ARCHIVE_TOKEN),
      "raw token in error: {message}"
    );
    assert!(
      !message.contains(&basic),
      "Basic credential in error: {message}"
    );
  }
  Ok(())
}

/// #76 76-S3: every archive failure status is bounded — only 401 and 403 earn
/// a credential-refresh retry — neither the raw scoped token nor its
/// Basic-encoded form reaches a log line or an infrastructure error, and no
/// usable or half-written cache entry survives.
///
/// The bound, not an exact count, is the contract: the two resolvers (setup
/// prefetch and the step itself) share one in-flight download, so whether the
/// second one re-requests the archive or joins the first depends on which
/// finishes first.
#[tokio::test]
async fn archive_failure_statuses_are_bounded_and_leave_no_cache_entry() -> TestResult {
  let basic = base64::Engine::encode(
    &base64::engine::general_purpose::STANDARD,
    format!("x-access-token:{ARCHIVE_TOKEN}"),
  );
  for (status, most) in [
    (StatusCode::NOT_FOUND, 2),
    (StatusCode::TOO_MANY_REQUESTS, 2),
    (StatusCode::INTERNAL_SERVER_ERROR, 2),
    (StatusCode::SERVICE_UNAVAILABLE, 2),
    (StatusCode::UNAUTHORIZED, 4),
    (StatusCode::FORBIDDEN, 4),
  ] {
    let peer = serve(Launch::Resolve(Archive::Status(status))).await?;
    let message = job(&peer.base)?;
    let (dir, _masker, observed) = run_observed(message).await?;
    assert_eq!(observed.conclusion, Some(Conclusion::Failure), "{status}");
    let attempts = peer.seen("/archive").len();
    assert!(
      (1..=most).contains(&attempts),
      "{status}: {attempts} archive requests is outside the bound of {most}"
    );
    for text in observed
      .infrastructure
      .iter()
      .map(|(_, _, message)| message)
      .chain(observed.logs.iter())
    {
      assert!(
        !text.contains(ARCHIVE_TOKEN),
        "{status}: raw token in {text}"
      );
      assert!(
        !text.contains(&basic),
        "{status}: Basic credential in {text}"
      );
    }
    assert!(
      extracted_manifests(&dir.path().join("data"))?.is_empty(),
      "{status}: a failed download left a usable cache entry"
    );
    let actions = dir.path().join("data/actions");
    let staging: Vec<_> = std::fs::read_dir(&actions)
      .into_iter()
      .flatten()
      .flatten()
      .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp-"))
      .map(|entry| entry.path())
      .collect();
    assert!(
      staging.is_empty(),
      "{status}: staging left behind: {staging:?}"
    );
  }
  Ok(())
}
