//! Issue 88: runner-side action fetch failures carry upstream's
//! `infrastructureFailureCategory` (`resolve_action`, `error_download_action`,
//! `invalid_action_download`); user errors carry none.
//!
//! The job is the sanitized github.com acquisition of `completejob-88.yml`'s
//! `steps-toolu` job (run 38098611227) with its captured `actions/checkout@v4`
//! step. Its launch endpoint is pointed at local **fault-injection** HTTP
//! servers standing in for GitHub: the injected statuses and cut streams are
//! the faults under test, never a fabricated successful service. Archives are
//! real gzip tarballs built here (or deliberately corrupt bytes).

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

/// What the fault-injection archive endpoint serves on each request.
#[derive(Clone, Copy)]
enum Archive {
  Status(u16),
  /// 401 on the first request, a valid action afterwards.
  UnauthorizedOnce,
  /// A valid gzip header, then the connection drops mid-body.
  CutMidBody,
  Corrupt,
}

/// What the launch download-info endpoint answers.
#[derive(Clone, Copy)]
enum Launch {
  Status(u16),
  Resolve(Archive),
}

fn action_tarball() -> TestResult<Vec<u8>> {
  let manifest = b"name: probe\nruns:\n  using: composite\n  steps:\n    - shell: bash\n      run: echo fetched\n";
  let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
    Vec::new(),
    flate2::Compression::default(),
  ));
  let mut header = tar::Header::new_gnu();
  header.set_size(u64::try_from(manifest.len())?);
  header.set_mode(0o644);
  header.set_cksum();
  builder.append_data(
    &mut header,
    "actions-checkout-0123456/action.yml",
    &manifest[..],
  )?;
  Ok(builder.into_inner()?.finish()?)
}

async fn serve(launch: Launch) -> TestResult<String> {
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
  let base = format!("http://{}", listener.local_addr()?);
  let tarball = action_tarball()?;
  let archive_hits = Arc::new(AtomicUsize::new(0));
  let tar_url = format!("{base}/archive");
  let launch_route = move || {
    let tar_url = tar_url.clone();
    async move {
      match launch {
        Launch::Status(code) => StatusCode::from_u16(code)
          .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
          .into_response(),
        Launch::Resolve(_) => axum::Json(serde_json::json!({"actions": {
          "actions/checkout@v4": {
            "resolved_name": "actions/checkout",
            "resolved_sha": SHA,
            "tar_url": tar_url,
          }
        }}))
        .into_response(),
      }
    }
  };
  let archive_route = move || {
    let hits = Arc::clone(&archive_hits);
    let tarball = tarball.clone();
    async move {
      let first = hits.fetch_add(1, Ordering::SeqCst) == 0;
      let Launch::Resolve(archive) = launch else {
        return StatusCode::NOT_FOUND.into_response();
      };
      match archive {
        Archive::Status(code) => StatusCode::from_u16(code)
          .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
          .into_response(),
        Archive::UnauthorizedOnce if first => StatusCode::UNAUTHORIZED.into_response(),
        Archive::UnauthorizedOnce => tarball.into_response(),
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
  Ok(base)
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

struct Observed {
  conclusion: Option<Conclusion>,
  /// `(step id, category, message)` of every infrastructure error.
  infrastructure: Vec<(String, String, String)>,
  steps: HashMap<String, Conclusion>,
}

async fn run(message: AgentJobRequestMessage) -> TestResult<Observed> {
  let dir = tempfile::tempdir()?;
  let config = RunnerConfig {
    data_dir: dir.path().join("data"),
    workspace_root: dir.path().join("work"),
    workspace_gc_hours: 0,
    ..RunnerConfig::default()
  };
  let workspace = config.workspace_root.join(&message.job_id);
  seed_nested_composite(&workspace)?;
  let runner = Runner::new(config, Arc::new(Mutex::new(SecretMasker::new())));
  let mut stream = runner.execute_job(message, CancellationToken::new());
  let mut observed = Observed {
    conclusion: None,
    infrastructure: Vec::new(),
    steps: HashMap::new(),
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
    }
  })
  .await?;
  Ok(observed)
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

async fn category_for(launch: Launch) -> TestResult<(Observed, String)> {
  let base = serve(launch).await?;
  let message = job(&base)?;
  let checkout = message.steps.first().ok_or("checkout missing")?.id.clone();
  Ok((run(message).await?, checkout))
}

#[tokio::test]
async fn runner_side_fetch_failures_carry_upstream_categories() -> TestResult {
  for (launch, category) in [
    (Launch::Status(500), "resolve_action"),
    (Launch::Status(429), "resolve_action"),
    (
      Launch::Resolve(Archive::Status(500)),
      "error_download_action",
    ),
    (
      Launch::Resolve(Archive::Status(404)),
      "error_download_action",
    ),
    (
      Launch::Resolve(Archive::Status(401)),
      "error_download_action",
    ),
    (
      Launch::Resolve(Archive::CutMidBody),
      "error_download_action",
    ),
    (Launch::Resolve(Archive::Corrupt), "invalid_action_download"),
  ] {
    let (observed, checkout) = category_for(launch).await?;
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
    (Launch::Status(422), Conclusion::Failure),
    // AccessDeniedException is the workflow's, not the runner's.
    (Launch::Resolve(Archive::Status(403)), Conclusion::Failure),
    // One auth refresh recovers a 401; nothing failed.
    (
      Launch::Resolve(Archive::UnauthorizedOnce),
      Conclusion::Success,
    ),
  ] {
    let (observed, _) = category_for(launch).await?;
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
  let base = serve(Launch::Status(500)).await?;
  let mut message = job(&base)?;
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
  let base = serve(Launch::Resolve(Archive::Status(503))).await?;
  let mut message = job(&base)?;
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
