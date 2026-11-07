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
