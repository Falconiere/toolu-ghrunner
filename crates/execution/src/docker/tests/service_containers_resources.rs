//! Real Docker coverage for service options, unrelated resources and registry auth.
//! Included by the service-container integration target; helpers replay real jobs.

use super::{TestResult, assert_services_removed, options, replay, service_job};
use shared::{ActionStep, AgentJobRequestMessage, Conclusion, RunnerEvent};

#[cfg(target_os = "linux")]
fn set_service_field(
  job: &mut AgentJobRequestMessage,
  name: &str,
  value: serde_json::Value,
) -> TestResult {
  let service = job
    .job_service_containers
    .as_mut()
    .and_then(|token| token.d.as_mut())
    .and_then(|map| map.first_mut())
    .ok_or("missing service")?;
  let fields = service.value.d.as_mut().ok_or("missing service fields")?;
  let value: shared::TemplateToken = serde_json::from_value(value)?;
  if let Some(entry) = fields
    .iter_mut()
    .find(|entry| entry.key.to_string_value() == Some(name))
  {
    entry.value = value;
  } else {
    fields.push(shared::DictEntry {
      key: shared::TemplateToken {
        token_type: 0,
        lit: Some(name.to_owned()),
        ..Default::default()
      },
      value,
    });
  }
  Ok(())
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires real Linux Docker daemon and nginx image access"]
async fn services_multiple_evaluated_env_volumes_and_unrelated_resources_survive() -> TestResult {
  let base = std::env::var_os("TOOLU_CONTAINER_TEST_ROOT")
    .map_or_else(std::env::temp_dir, std::path::PathBuf::from);
  let volume = tempfile::tempdir_in(base)?;
  std::fs::write(volume.path().join("value"), "real-volume-value")?;
  let mut job = service_job()?;
  set_service_field(
    &mut job,
    "env",
    serde_json::json!({"type":2,"map":[{"key":{"type":0,"lit":"EVALUATED"},"value":{"type":3,"expr":"format('service-{0}', 74)"}}]}),
  )?;
  set_service_field(
    &mut job,
    "volumes",
    serde_json::json!({"type":1,"seq":[{"type":0,"lit":format!("{}:/data:ro",volume.path().display())}]}),
  )?;
  options(
    &mut job,
    "--hostname configured-service --cpus 0.5 --health-cmd 'test \"$EVALUATED\" = service-74 && test \"$(cat /data/value)\" = real-volume-value && test \"$(hostname)\" = configured-service' --health-interval 1s --health-retries 2",
  )?;
  let map = job
    .job_service_containers
    .as_mut()
    .and_then(|token| token.d.as_mut())
    .ok_or("missing services")?;
  let mut second = map.first().ok_or("missing web")?.clone();
  second.key.lit = Some("second".to_owned());
  map.push(second);
  job.steps = vec![ActionStep::script(
    "client",
    r"
test '${{ job.services.web.id }}' != '${{ job.services.second.id }}'
test '${{ job.services.web.network }}' = '${{ job.services.second.network }}'
curl -fsS 'http://127.0.0.1:${{ job.services.web.ports[80] }}' > first
curl -fsS 'http://127.0.0.1:${{ job.services.second.ports[80] }}' > second
",
    "",
  )];
  let docker = bollard::Docker::connect_with_local_defaults()?;
  let sentinel = docker
    .create_network(bollard::models::NetworkCreateRequest {
      name: format!("unrelated_74_{}", uuid::Uuid::new_v4().simple()),
      ..Default::default()
    })
    .await?;
  let sentinel_container = docker
    .create_container(
      None,
      bollard::models::ContainerCreateBody {
        image: Some("nginx:1.27-alpine".to_owned()),
        host_config: Some(bollard::models::HostConfig {
          network_mode: Some(sentinel.id.clone()),
          ..Default::default()
        }),
        ..Default::default()
      },
    )
    .await?;
  let result = replay(job).await;
  let container_survives = docker
    .inspect_container(&sentinel_container.id, None)
    .await
    .is_ok();
  docker
    .remove_container(
      &sentinel_container.id,
      Some(bollard::query_parameters::RemoveContainerOptions {
        force: true,
        ..Default::default()
      }),
    )
    .await?;
  let survives = docker.inspect_network(&sentinel.id, None).await.is_ok();
  docker.remove_network(&sentinel.id).await?;
  // Always remove the sentinel resources before propagating replay failures.
  // Preserve their survival evidence in that error as well as on the happy path.
  let (_, events) = result.map_err(|error| {
    format!(
      "service replay failed: {error}; sentinel container survives: {container_survives}; sentinel network survives: {survives}"
    )
  })?;
  assert!(survives);
  assert!(container_survives);
  assert!(
    events.iter().any(|event| matches!(
      event,
      RunnerEvent::JobCompleted {
        conclusion: Conclusion::Success,
        ..
      }
    )),
    "{events:?}"
  );
  assert_eq!(
    std::fs::read_to_string(volume.path().join("value"))?,
    "real-volume-value"
  );
  assert_services_removed(&events).await
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires real Linux Docker daemon, host networking and nginx image access"]
async fn services_fixed_port_conflict_is_actionable() -> TestResult {
  let listener = std::net::TcpListener::bind("0.0.0.0:0")?;
  let port = listener.local_addr()?.port();
  let mut job = service_job()?;
  set_service_field(
    &mut job,
    "ports",
    serde_json::json!({"type":1,"seq":[{"type":0,"lit":format!("{port}:80")}]}),
  )?;
  let (_, events) = replay(job).await?;
  assert!(
    events.iter().any(|event| matches!(
      event,
      RunnerEvent::JobCompleted {
        conclusion: Conclusion::Failure,
        ..
      }
    )),
    "{events:?}"
  );
  assert!(
    !events
      .iter()
      .any(|event| matches!(event, RunnerEvent::StepStarted { .. }))
  );
  assert!(events.iter().any(|event| matches!(event, RunnerEvent::Log {line,..} if line.contains("start service container"))), "{events:?}");
  assert_services_removed(&events).await
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires authenticated disposable registry provisioned by service_containers_linux.sh"]
async fn services_private_registry_auth_and_masked_credentials() -> TestResult {
  let image = std::env::var("TOOLU_SERVICE_REGISTRY_IMAGE")?;
  for (password, expected) in [
    ("service-password", Conclusion::Success),
    ("wrong-service-password", Conclusion::Failure),
  ] {
    let mut job = service_job()?;
    set_service_field(&mut job, "image", serde_json::json!({"type":0,"lit":image}))?;
    set_service_field(
      &mut job,
      "credentials",
      serde_json::json!({"type":2,"map":[
        {"key":{"type":0,"lit":"username"},"value":{"type":0,"lit":"service-user"}},
        {"key":{"type":0,"lit":"password"},"value":{"type":0,"lit":password}}
      ]}),
    )?;
    job.steps = vec![ActionStep::script(
      "client",
      "printf 'SERVICE_AUTH service-user service-password\\n'",
      "",
    )];
    let (_, events) = replay(job).await?;
    assert!(events.iter().any(|event| matches!(event, RunnerEvent::JobCompleted {conclusion,..} if *conclusion == expected)), "{events:?}");
    for event in &events {
      if let RunnerEvent::Log { line, .. } = event {
        assert!(!line.contains(password));
        assert!(!line.contains("service-user"));
      }
    }
    if expected == Conclusion::Success {
      assert!(events.iter().any(|event| matches!(event, RunnerEvent::Log {line,..} if line.trim() == "SERVICE_AUTH *** ***")), "{events:?}");
      assert_services_removed(&events).await?;
    } else {
      assert!(
        !events
          .iter()
          .any(|event| matches!(event, RunnerEvent::StepStarted { .. }))
      );
    }
  }
  Ok(())
}
