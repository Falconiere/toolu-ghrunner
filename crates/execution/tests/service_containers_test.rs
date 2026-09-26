//! Production service-container replay against a real Docker daemon.
//!
//! Ungated cases cover malformed and empty declarations; non-Linux hosts also
//! verify explicit rejection. Linux Docker cases are ignored by the default
//! suite and run explicitly via `scripts/test/service_containers_linux.sh`, which
//! provisions the private registry needed by the credential test.
//! `service_job` varies a captured envelope; `replay` collects and masks actual
//! runner events. Other helpers edit declarations, trigger observed cancellation,
//! and inspect Docker to verify resource cleanup.

use std::borrow::Cow;
use std::sync::{Arc, Mutex};

use execution::Runner;
use shared::{
  ActionStep, AgentJobRequestMessage, Conclusion, RunnerConfig, RunnerEvent, SecretMasker,
};
use tokio_util::sync::CancellationToken;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn service_job() -> Result<AgentJobRequestMessage, Box<dyn std::error::Error>> {
  let mut value: serde_json::Value = serde_json::from_str(include_str!(
    "../../toolu-runner/tests/fixtures/job_container_message.json"
  ))?;
  let object = value
    .as_object_mut()
    .ok_or("captured job is not an object")?;
  object.insert("jobContainer".to_owned(), serde_json::Value::Null);
  // Explicit replay variation: preserve the captured envelope while adding
  // the upstream service mapping shape. This is not a service capture claim.
  object.insert("jobServiceContainers".to_owned(), serde_json::json!({"type":2,"map":[
    {"key":{"type":0,"lit":"web"},"value":{"type":2,"map":[
      {"key":{"type":0,"lit":"image"},"value":{"type":0,"lit":"nginx:1.27-alpine"}},
      {"key":{"type":0,"lit":"ports"},"value":{"type":1,"seq":[{"type":0,"lit":"80"}]}},
      {"key":{"type":0,"lit":"options"},"value":{"type":0,"lit":"--health-cmd 'wget -q -O /dev/null http://localhost' --health-interval 1s --health-retries 3"}}
    ]}}
  ]}));
  let mut job: AgentJobRequestMessage = serde_json::from_value(value)?;
  job.steps = vec![ActionStep::script(
    "client",
    "printf user-step-ran > ran",
    "",
  )];
  Ok(job)
}

async fn replay(
  job: AgentJobRequestMessage,
) -> Result<(tempfile::TempDir, Vec<RunnerEvent>), Box<dyn std::error::Error>> {
  let base = std::env::var_os("TOOLU_CONTAINER_TEST_ROOT")
    .map_or_else(std::env::temp_dir, std::path::PathBuf::from);
  let root = tempfile::tempdir_in(base)?;
  let config = RunnerConfig {
    data_dir: root.path().join("data"),
    workspace_root: root.path().join("work"),
    workspace_gc_hours: 0,
    ..RunnerConfig::default()
  };
  let runner = Runner::new(config, Arc::new(Mutex::new(SecretMasker::new())));
  let mut receiver = runner.execute_job(job, CancellationToken::new());
  let mut events = Vec::new();
  while let Some(mut event) = receiver.recv().await {
    if let RunnerEvent::Log { line, .. } = &mut event {
      let masker = runner
        .masker()
        .lock()
        .map_err(|poisoned| format!("masker poisoned: {poisoned}"))?;
      if let Cow::Owned(masked) = masker.mask(line) {
        *line = masked;
      }
    }
    events.push(event);
  }
  Ok((root, events))
}

#[cfg(not(target_os = "linux"))]
#[tokio::test]
async fn services_reject_unsupported_host_before_user_steps() -> TestResult {
  let (root, events) = replay(service_job()?).await?;
  assert!(events.iter().any(|event| matches!(
    event,
    RunnerEvent::JobCompleted {
      conclusion: Conclusion::Failure,
      ..
    }
  )));
  assert!(
    !events
      .iter()
      .any(|event| matches!(event, RunnerEvent::StepStarted { .. }))
  );
  assert!(
    !root
      .path()
      .join("work")
      .join(service_job()?.job_id)
      .join("ran")
      .exists()
  );
  Ok(())
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires real Linux Docker daemon and nginx image access"]
async fn services_host_dynamic_port_connects_and_cleans_up() -> TestResult {
  let mut job = service_job()?;
  job.steps = vec![ActionStep::script(
    "client",
    r"
test -n '${{ job.services.web.id }}'
test -n '${{ job.services.web.network }}'
curl --fail --silent 'http://127.0.0.1:${{ job.services.web.ports[80] }}' > response
printf '%s\n%s\n' '${{ job.services.web.id }}' '${{ job.services.web.network }}' > resources
",
    "",
  )];
  let job_id = job.job_id.clone();
  let (root, events) = replay(job).await?;
  assert!(
    events.iter().any(|event| matches!(
      event,
      RunnerEvent::JobCompleted {
        conclusion: Conclusion::Success,
        ..
      }
    )),
    "job did not succeed: {events:?}"
  );
  let workspace = root.path().join("work").join(job_id);
  assert!(std::fs::read_to_string(workspace.join("response"))?.contains("Welcome to nginx"));
  let ids = std::fs::read_to_string(workspace.join("resources"))?;
  let mut ids = ids.lines();
  let docker = bollard::Docker::connect_with_local_defaults()?;
  assert!(matches!(
    docker
      .inspect_container(ids.next().ok_or("missing container")?, None)
      .await,
    Err(bollard::errors::Error::DockerResponseServerError {
      status_code: 404,
      ..
    })
  ));
  assert!(matches!(
    docker
      .inspect_network(ids.next().ok_or("missing network")?, None)
      .await,
    Err(bollard::errors::Error::DockerResponseServerError {
      status_code: 404,
      ..
    })
  ));
  Ok(())
}

#[tokio::test]
async fn malformed_services_fail_before_user_steps() -> TestResult {
  let mut job = service_job()?;
  job.job_service_containers = Some(shared::TemplateToken {
    token_type: 0,
    lit: Some("invalid services mapping".to_owned()),
    ..Default::default()
  });
  let (_, events) = replay(job).await?;
  assert!(events.iter().any(|event| matches!(
    event,
    RunnerEvent::JobCompleted {
      conclusion: Conclusion::Failure,
      ..
    }
  )));
  assert!(
    !events
      .iter()
      .any(|event| matches!(event, RunnerEvent::StepStarted { .. }))
  );
  Ok(())
}

#[tokio::test]
async fn empty_services_do_not_require_docker() -> TestResult {
  let mut job = service_job()?;
  job.job_service_containers = Some(shared::TemplateToken {
    token_type: 2,
    d: Some(Vec::new()),
    ..Default::default()
  });
  let job_id = job.job_id.clone();
  let (root, events) = replay(job).await?;
  assert!(events.iter().any(|event| matches!(
    event,
    RunnerEvent::JobCompleted {
      conclusion: Conclusion::Success,
      ..
    }
  )));
  assert_eq!(
    std::fs::read_to_string(root.path().join("work").join(job_id).join("ran"))?,
    "user-step-ran"
  );
  Ok(())
}

#[cfg(target_os = "linux")]
fn options(job: &mut AgentJobRequestMessage, value: &str) -> TestResult {
  let services = job
    .job_service_containers
    .as_mut()
    .ok_or("missing services")?;
  let service = services
    .d
    .as_mut()
    .and_then(|map| map.first_mut())
    .ok_or("missing service")?;
  let option = service
    .value
    .d
    .as_mut()
    .and_then(|map| {
      map
        .iter_mut()
        .find(|entry| entry.key.to_string_value() == Some("options"))
    })
    .ok_or("missing options")?;
  option.value.lit = Some(value.to_owned());
  Ok(())
}

#[cfg(target_os = "linux")]
async fn assert_services_removed(events: &[RunnerEvent]) -> TestResult {
  let docker = bollard::Docker::connect_with_local_defaults()?;
  let mut count = 0;
  for event in events {
    if let RunnerEvent::Log { line, .. } = event
      && line.starts_with("Service ")
      && line.contains(" created: ")
    {
      let (_, rest) = line
        .split_once(" created: ")
        .ok_or_else(|| format!("invalid start log: {line}"))?;
      let (id, network) = rest
        .split_once(" on ")
        .ok_or_else(|| format!("missing network in start log: {line}"))?;
      assert!(matches!(
        docker.inspect_container(id, None).await,
        Err(bollard::errors::Error::DockerResponseServerError {
          status_code: 404,
          ..
        })
      ));
      assert!(matches!(
        docker.inspect_network(network, None).await,
        Err(bollard::errors::Error::DockerResponseServerError {
          status_code: 404,
          ..
        })
      ));
      count += 1;
    }
  }
  assert!(count > 0, "test did not observe a started service");
  Ok(())
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires real Linux Docker daemon and nginx image access"]
async fn services_container_dns_connects_on_shared_network() -> TestResult {
  let mut job = service_job()?;
  job.job_container = Some(shared::TemplateToken {
    token_type: 0,
    lit: Some("alpine:3.21".to_owned()),
    ..Default::default()
  });
  job.steps = vec![ActionStep::script(
    "client",
    r"
test '${{ job.services.web.network }}' = '${{ job.container.network }}'
wget -q -O response http://web
printf '%s' '${{ job.container.id }}' > job-container
",
    "",
  )];
  let job_id = job.job_id.clone();
  let (root, events) = replay(job).await?;
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
  let workspace = root.path().join("work").join(job_id);
  assert!(std::fs::read_to_string(workspace.join("response"))?.contains("Welcome to nginx"));
  let docker = bollard::Docker::connect_with_local_defaults()?;
  assert!(matches!(
    docker
      .inspect_container(
        &std::fs::read_to_string(workspace.join("job-container"))?,
        None
      )
      .await,
    Err(bollard::errors::Error::DockerResponseServerError {
      status_code: 404,
      ..
    })
  ));
  assert_services_removed(&events).await
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires real Linux Docker daemon and nginx image access"]
async fn services_unhealthy_prevents_steps_and_retains_logs() -> TestResult {
  let mut job = service_job()?;
  options(
    &mut job,
    "--health-cmd false --health-interval 1s --health-retries 1",
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
  assert!(events.iter().any(
    |event| matches!(event, RunnerEvent::Log {line,..} if line.contains("reported unhealthy"))
  ));
  assert!(
    events.iter().any(
      |event| matches!(event, RunnerEvent::Log {line,..} if line.starts_with("[service web]"))
    )
  );
  assert_services_removed(&events).await
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires real Linux Docker daemon and nginx image access"]
async fn services_delayed_health_and_no_healthcheck_permit_steps() -> TestResult {
  for option in [
    "--health-cmd 'test -e /tmp/ready || (touch /tmp/ready; false)' --health-interval 1s --health-retries 5",
    "--no-healthcheck",
  ] {
    let mut job = service_job()?;
    options(&mut job, option)?;
    let (_, events) = replay(job).await?;
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
    assert_services_removed(&events).await?;
  }
  Ok(())
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires real Linux Docker daemon and nginx image access"]
async fn services_second_start_failure_cleans_first() -> TestResult {
  let mut job = service_job()?;
  let map = job
    .job_service_containers
    .as_mut()
    .and_then(|token| token.d.as_mut())
    .ok_or("missing services")?;
  let mut second = map.first().ok_or("missing web")?.clone();
  second.key.lit = Some("second".to_owned());
  let fields = second.value.d.as_mut().ok_or("missing fields")?;
  let image = fields
    .iter_mut()
    .find(|entry| entry.key.to_string_value() == Some("image"))
    .ok_or("missing image")?;
  image.value.lit = Some("127.0.0.1:1/service-does-not-exist:74".to_owned());
  map.push(second);
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
  assert_services_removed(&events).await
}

#[cfg(target_os = "linux")]
async fn cancel_observed(mut job: AgentJobRequestMessage, during_health: bool) -> TestResult {
  let base = std::env::var_os("TOOLU_CONTAINER_TEST_ROOT")
    .map_or_else(std::env::temp_dir, std::path::PathBuf::from);
  let root = tempfile::tempdir_in(base)?;
  let config = RunnerConfig {
    data_dir: root.path().join("data"),
    workspace_root: root.path().join("work"),
    workspace_gc_hours: 0,
    ..RunnerConfig::default()
  };
  if during_health {
    options(
      &mut job,
      "--health-cmd false --health-start-period 1h --health-interval 1s",
    )?;
  } else {
    job.steps = vec![ActionStep::script(
      "wait",
      "echo CLIENT_RUNNING; sleep 60",
      "",
    )];
  }
  let runner = Runner::new(config, Arc::new(Mutex::new(SecretMasker::new())));
  let cancel = CancellationToken::new();
  let mut receiver = runner.execute_job(job, cancel.clone());
  let mut observed = false;
  let mut events = Vec::new();
  while let Some(event) = receiver.recv().await {
    if let RunnerEvent::Log { line, .. } = &event {
      let trigger = if during_health {
        line.contains("is starting; waiting")
      } else {
        line.trim() == "CLIENT_RUNNING"
      };
      if trigger {
        observed = true;
        cancel.cancel();
      }
    }
    events.push(event);
  }
  assert!(observed, "cancellation trigger was not observed");
  assert!(
    events.iter().any(|event| matches!(
      event,
      RunnerEvent::JobCompleted {
        conclusion: Conclusion::Cancelled,
        ..
      }
    )),
    "cancel did not complete as Cancelled"
  );
  if during_health {
    assert!(
      !events
        .iter()
        .any(|event| matches!(event, RunnerEvent::StepStarted { .. }))
    );
  }
  assert_services_removed(&events).await
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires real Linux Docker daemon and nginx image access"]
async fn services_cancellation_during_health_and_job_cleans_resources() -> TestResult {
  cancel_observed(service_job()?, true).await?;
  cancel_observed(service_job()?, false).await
}

#[cfg(target_os = "linux")]
#[path = "../src/docker/tests/service_containers_resources.rs"]
mod resources;
