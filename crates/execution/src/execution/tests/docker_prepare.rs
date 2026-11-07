//! Capture-driven Docker setup discovery checks.

use super::*;

#[tokio::test]
async fn duplicate_captured_registry_image_needs_one_pull() -> Result<(), Box<dyn std::error::Error>>
{
  let job: shared::AgentJobRequestMessage = serde_json::from_str(include_str!(
    "../../../tests/compat_103_docker_message.json"
  ))?;
  let step = job
    .steps
    .iter()
    .find(|step| step.reference.ref_type.as_deref() == Some("containerRegistry"))
    .ok_or("captured Docker step missing")?;
  let mut duplicate = step.clone();
  duplicate.reference.image = duplicate
    .reference
    .image
    .map(|image| image.to_ascii_uppercase());
  let fetcher = ActionFetcher::new(&shared::NetworkConfig::default())?;
  let plan = discover(
    &[step.clone(), duplicate],
    &Discovery {
      fetcher: &fetcher,
      client: &reqwest::Client::new(),
      data_dir: Path::new(""),
    },
    &CancellationToken::new(),
  )
  .await?;
  assert_eq!(
    plan.pulls,
    vec![step.reference.image.clone().ok_or("image")?]
  );
  assert!(plan.builds.is_empty());
  Ok(())
}

#[tokio::test]
async fn nested_remote_composites_discover_deduplicated_pulls_before_builds()
-> Result<(), Box<dyn std::error::Error>> {
  use std::io::Write;
  use std::sync::Mutex;

  const SHA: &str = "8bcd8e1af3c095561f1043123848fc8b2db0f189";
  let mut archive = tar::Builder::new(flate2::write::GzEncoder::new(
    Vec::new(),
    flate2::Compression::default(),
  ));
  for (path, content) in [
    (
      "probe/outer/action.yml",
      "name: outer\nruns:\n  using: composite\n  steps:\n    - uses: acme/probe/inner@main\n",
    ),
    (
      "probe/inner/action.yml",
      "name: inner\nruns:\n  using: composite\n  steps:\n    - uses: acme/probe/docker@main\n    - uses: docker://alpine:3.21\n    - uses: docker://ALPINE:3.21\n",
    ),
    (
      "probe/docker/action.yml",
      "name: docker\nruns:\n  using: docker\n  image: Dockerfile\n",
    ),
    ("probe/docker/Dockerfile", "FROM scratch\n"),
  ] {
    let mut header = tar::Header::new_gnu();
    header.set_size(u64::try_from(content.len())?);
    header.set_mode(0o644);
    header.set_cksum();
    archive.append_data(&mut header, path, content.as_bytes())?;
  }
  let mut encoder = archive.into_inner()?;
  encoder.flush()?;
  let body = encoder.finish()?;
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
  let api = format!("http://{}", listener.local_addr()?);
  let router = axum::Router::new()
    .route(
      "/repos/acme/probe/commits/main",
      axum::routing::get(|| async { format!("{{\"sha\":\"{SHA}\"}}") }),
    )
    .route(
      &format!("/repos/acme/probe/tarball/{SHA}"),
      axum::routing::get(move || {
        let body = body.clone();
        async move { body }
      }),
    );
  let server = tokio::spawn(async move {
    let _ = axum::serve(listener, router).await;
  });
  let mut raw: serde_json::Value = serde_json::from_str(include_str!(
    "../../../tests/compat_103_docker_message.json"
  ))?;
  let github = raw
    .get_mut("contextData")
    .and_then(|v| v.get_mut("github"))
    .and_then(|v| v.get_mut("d"))
    .and_then(serde_json::Value::as_array_mut)
    .ok_or("github context")?;
  let api_entry = github
    .iter_mut()
    .find(|entry| entry.get("k").and_then(serde_json::Value::as_str) == Some("api_url"))
    .ok_or("api_url")?;
  *api_entry.get_mut("v").ok_or("api_url value")? = api.into();
  let mut job: shared::AgentJobRequestMessage = serde_json::from_value(raw)?;
  job.variables.remove("system.github.launch_endpoint");
  let mut outer = ActionStep::with_ref_type("outer", "repository");
  outer.reference.name = Some("acme/probe".to_owned());
  outer.reference.path = Some("outer".to_owned());
  outer.reference.git_ref = Some("main".to_owned());
  outer.reference.repository_type = Some("GitHub".to_owned());
  job.steps = vec![outer];
  let fetcher = ActionFetcher::for_job(
    &job,
    Arc::new(Mutex::new(shared::SecretMasker::new())),
    CancellationToken::new(),
    &shared::NetworkConfig::default(),
  )?;
  let dir = tempfile::tempdir()?;
  let plan = discover(
    &job.steps,
    &Discovery {
      fetcher: &fetcher,
      client: &reqwest::Client::new(),
      data_dir: dir.path(),
    },
    &CancellationToken::new(),
  )
  .await?;
  server.abort();
  assert!(matches!(
    setup_step_number(usize::MAX),
    Err(RunnerError::StepExecution(_))
  ));
  assert_eq!(plan.pulls, ["alpine:3.21"]);
  assert_eq!(plan.builds.len(), 1);
  let build = plan.builds.first().ok_or("missing build")?;
  assert_eq!(build.repository, "acme/probe");
  assert_eq!(build.actions.len(), 1);
  assert_eq!(
    build.actions.first().ok_or("missing action")?.image,
    "Dockerfile"
  );
  Ok(())
}

const FOLLOW_UP_SHA: &str = "8bcd8e1af3c095561f1043123848fc8b2db0f189";

fn follow_up_archive(files: &[(String, String)]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
  use std::io::Write;

  let mut archive = tar::Builder::new(flate2::write::GzEncoder::new(
    Vec::new(),
    flate2::Compression::default(),
  ));
  for (path, content) in files {
    let mut header = tar::Header::new_gnu();
    header.set_size(u64::try_from(content.len())?);
    header.set_mode(0o644);
    header.set_cksum();
    archive.append_data(&mut header, path, content.as_bytes())?;
  }
  let mut encoder = archive.into_inner()?;
  encoder.flush()?;
  Ok(encoder.finish()?)
}

async fn follow_up_server(
  body: Vec<u8>,
  hold: Option<(tokio::sync::mpsc::Sender<()>, Arc<tokio::sync::Notify>)>,
) -> Result<(String, tokio::task::JoinHandle<()>), Box<dyn std::error::Error>> {
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
  let api = format!("http://{}", listener.local_addr()?);
  let router = axum::Router::new()
    .route(
      "/repos/acme/probe/commits/main",
      axum::routing::get(|| async { format!("{{\"sha\":\"{FOLLOW_UP_SHA}\"}}") }),
    )
    .route(
      &format!("/repos/acme/probe/tarball/{FOLLOW_UP_SHA}"),
      axum::routing::get(move || {
        let body = body.clone();
        let hold = hold.clone();
        async move {
          if let Some((arrived, release)) = hold {
            let _ = arrived.send(()).await;
            release.notified().await;
          }
          body
        }
      }),
    );
  let server = tokio::spawn(async move {
    let _ = axum::serve(listener, router).await;
  });
  Ok((api, server))
}

fn follow_up_job(
  api: &str,
  steps: Vec<ActionStep>,
) -> Result<shared::AgentJobRequestMessage, Box<dyn std::error::Error>> {
  let mut raw: serde_json::Value = serde_json::from_str(include_str!(
    "../../../tests/compat_103_docker_message.json"
  ))?;
  let github = raw
    .get_mut("contextData")
    .and_then(|v| v.get_mut("github"))
    .and_then(|v| v.get_mut("d"))
    .and_then(serde_json::Value::as_array_mut)
    .ok_or("github context")?;
  let api_entry = github
    .iter_mut()
    .find(|entry| entry.get("k").and_then(serde_json::Value::as_str) == Some("api_url"))
    .ok_or("api_url")?;
  *api_entry.get_mut("v").ok_or("api_url value")? = api.into();
  let mut job: shared::AgentJobRequestMessage = serde_json::from_value(raw)?;
  job.variables.remove("system.github.launch_endpoint");
  job.steps = steps;
  Ok(job)
}

fn follow_up_step(path: &str) -> ActionStep {
  let mut step = ActionStep::with_ref_type(path, "repository");
  step.reference.name = Some("acme/probe".to_owned());
  step.reference.path = Some(path.to_owned());
  step.reference.git_ref = Some("main".to_owned());
  step.reference.repository_type = Some("GitHub".to_owned());
  step
}

fn follow_up_fetcher(job: &shared::AgentJobRequestMessage) -> Result<ActionFetcher, RunnerError> {
  ActionFetcher::for_job(
    job,
    Arc::new(std::sync::Mutex::new(shared::SecretMasker::new())),
    CancellationToken::new(),
    &shared::NetworkConfig::default(),
  )
}

#[tokio::test]
async fn graceful_cancel_interrupts_inflight_archive_discovery()
-> Result<(), Box<dyn std::error::Error>> {
  let body = follow_up_archive(&[(
    "probe/outer/action.yml".to_owned(),
    "name: outer\nruns:\n  using: docker\n  image: Dockerfile\n".to_owned(),
  )])?;
  let (arrived_tx, mut arrived_rx) = tokio::sync::mpsc::channel(1);
  let release = Arc::new(tokio::sync::Notify::new());
  let (api, server) = follow_up_server(body, Some((arrived_tx, Arc::clone(&release)))).await?;
  let job = follow_up_job(&api, vec![follow_up_step("outer")])?;
  let fetcher = follow_up_fetcher(&job)?;
  let cancel = CancellationToken::new();
  let signal = cancel.clone();
  let discovery = tokio::spawn(async move {
    let dir = tempfile::tempdir().map_err(|error| RunnerError::StepExecution(error.to_string()))?;
    discover(
      &job.steps,
      &Discovery {
        fetcher: &fetcher,
        client: &reqwest::Client::new(),
        data_dir: dir.path(),
      },
      &signal,
    )
    .await
  });
  tokio::time::timeout(std::time::Duration::from_secs(5), arrived_rx.recv())
    .await?
    .ok_or("archive request did not arrive")?;
  cancel.cancel();
  let result = tokio::time::timeout(std::time::Duration::from_secs(2), discovery).await??;
  release.notify_waiters();
  server.abort();
  assert!(matches!(result, Err(RunnerError::Cancelled)));
  Ok(())
}

#[tokio::test]
async fn repeated_composites_are_visited_once() -> Result<(), Box<dyn std::error::Error>> {
  let body = follow_up_archive(&[
    (
      "probe/outer/action.yml".to_owned(),
      "name: outer\nruns:\n  using: composite\n  steps:\n    - uses: acme/probe/outer@main\n    - uses: acme/probe/outer@main\n    - uses: acme/probe/docker@main\n".to_owned(),
    ),
    (
      "probe/docker/action.yml".to_owned(),
      "name: docker\nruns:\n  using: docker\n  image: Dockerfile\n".to_owned(),
    ),
    ("probe/docker/Dockerfile".to_owned(), "FROM scratch\n".to_owned()),
  ])?;
  let (api, server) = follow_up_server(body, None).await?;
  let job = follow_up_job(&api, vec![follow_up_step("outer"), follow_up_step("outer")])?;
  let fetcher = follow_up_fetcher(&job)?;
  let dir = tempfile::tempdir()?;
  let plan = discover(
    &job.steps,
    &Discovery {
      fetcher: &fetcher,
      client: &reqwest::Client::new(),
      data_dir: dir.path(),
    },
    &CancellationToken::new(),
  )
  .await?;
  server.abort();
  assert!(plan.pulls.is_empty());
  assert_eq!(plan.builds.len(), 1);
  assert_eq!(plan.builds.first().ok_or("build missing")?.actions.len(), 1);
  Ok(())
}

#[tokio::test]
async fn distinct_composite_chain_keeps_depth_limit() -> Result<(), Box<dyn std::error::Error>> {
  let files: Vec<_> = (0..=11)
    .map(|depth| {
      let next = if depth == 11 {
        "    - uses: docker://alpine:3.21\n".to_owned()
      } else {
        format!("    - uses: acme/probe/level{}@main\n", depth + 1)
      };
      (
        format!("probe/level{depth}/action.yml"),
        format!("name: level{depth}\nruns:\n  using: composite\n  steps:\n{next}"),
      )
    })
    .collect();
  let body = follow_up_archive(&files)?;
  let (api, server) = follow_up_server(body, None).await?;
  let job = follow_up_job(&api, vec![follow_up_step("level0")])?;
  let fetcher = follow_up_fetcher(&job)?;
  let dir = tempfile::tempdir()?;
  let result = discover(
    &job.steps,
    &Discovery {
      fetcher: &fetcher,
      client: &reqwest::Client::new(),
      data_dir: dir.path(),
    },
    &CancellationToken::new(),
  )
  .await;
  server.abort();
  assert!(
    matches!(result, Err(RunnerError::StepExecution(message)) if message.contains("composite action depth limit exceeded"))
  );
  Ok(())
}
