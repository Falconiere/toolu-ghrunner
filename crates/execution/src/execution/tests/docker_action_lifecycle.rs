//! Real-Docker network and cleanup cases included by `docker_action_linux_test`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bollard::models::{ContainerCreateBody, HostConfig};
use bollard::query_parameters::{
  CreateContainerOptions, InspectContainerOptions, InspectNetworkOptions, RemoveContainerOptions,
  StartContainerOptions,
};
use execution::Runner;
use shared::{
  ActionStep, AgentJobRequestMessage, Conclusion, MaskerRedactor, RunnerEvent, SecretMasker,
};
use tokio_util::sync::CancellationToken;

use super::{
  ALPINE, CAPTURE, TestResult, action_container_ids, captured_job, collect_job, config_for,
  is_missing, job_conclusion, linux_root, local_step, mask_event, pull_alpine, registry_step,
  seed_probe, service_token, wait_for_file,
};

#[tokio::test]
#[ignore = "requires Linux, real Docker, and a daemon-shared test root"]
async fn action_joins_job_network_and_reaches_real_peer() -> TestResult {
  let root = linux_root()?;
  let config = config_for(root.path());
  let mut job: AgentJobRequestMessage = serde_json::from_str(CAPTURE)?;
  let workspace = config.workspace_root.join(&job.job_id);
  seed_probe(&workspace, "network", "action.yml")?;
  let peer_name = format!("docker75-peer-{}", uuid::Uuid::new_v4().simple());
  job.steps = vec![
    ActionStep::script(
      "network_ready",
      r"
printf '%s' '${{ job.container.network }}' > docker-network-name
while test ! -f docker-peer-ready; do sleep 0.1; done
",
      "",
    ),
    local_step(
      "network_action",
      "./.github/actions/network",
      &[("marker", "NETWORK"), ("peer", &peer_name)],
    ),
    ActionStep::script(
      "network_cleanup",
      r"
: > docker-peer-remove
while test ! -f docker-peer-removed; do sleep 0.1; done
",
      "always()",
    ),
  ];
  let docker = bollard::Docker::connect_with_local_defaults()?;
  pull_alpine(&docker).await?;
  let runner = Runner::new(config, Arc::new(Mutex::new(SecretMasker::new())));
  let redactor = MaskerRedactor(Arc::clone(runner.masker()));
  let mut receiver = runner.execute_job(job, CancellationToken::new());
  let network_file = workspace.join("docker-network-name");
  wait_for_file(&network_file).await?;
  let network = tokio::fs::read_to_string(&network_file).await?;
  let created = docker
    .create_container(
      Some(CreateContainerOptions {
        name: Some(peer_name.clone()),
        ..Default::default()
      }),
      ContainerCreateBody {
        image: Some(ALPINE.to_owned()),
        cmd: Some(vec![
          "sh".to_owned(),
          "-c".to_owned(),
          "while true; do printf 'HTTP/1.1 200 OK\\r\\nContent-Length: 10\\r\\n\\r\\nnetwork-ok' | nc -l -p 8080; done".to_owned(),
        ]),
        host_config: Some(HostConfig {
          network_mode: Some(network.clone()),
          ..Default::default()
        }),
        ..Default::default()
      },
    )
    .await?;
  let peer_scenario: TestResult = async {
    docker
      .start_container(&created.id, None::<StartContainerOptions>)
      .await?;
    tokio::fs::write(workspace.join("docker-peer-ready"), "ready").await?;
    wait_for_file(&workspace.join("docker-peer-remove")).await
  }
  .await;
  let peer_cleanup = docker
    .remove_container(
      &created.id,
      Some(RemoveContainerOptions {
        force: true,
        ..Default::default()
      }),
    )
    .await;
  let ready_signal = tokio::fs::write(workspace.join("docker-peer-ready"), "ready").await;
  let removed_signal = tokio::fs::write(workspace.join("docker-peer-removed"), "removed").await;
  peer_scenario?;
  peer_cleanup?;
  ready_signal?;
  removed_signal?;
  let events = tokio::time::timeout(Duration::from_secs(180), async {
    let mut events = Vec::new();
    while let Some(mut event) = receiver.recv().await {
      mask_event(&redactor, &mut event);
      events.push(event);
    }
    events
  })
  .await?;
  assert_eq!(
    job_conclusion(&events),
    Some(Conclusion::Success),
    "{events:#?}"
  );
  assert_eq!(
    tokio::fs::read_to_string(workspace.join("docker-network.txt")).await?,
    "network-ok"
  );
  assert!(is_missing(
    &docker
      .inspect_network(&network, None::<InspectNetworkOptions>)
      .await
  ));
  Ok(())
}

#[tokio::test]
#[ignore = "requires Linux, real Docker, and a daemon-shared test root"]
async fn docker_action_joins_service_only_network_through_main_and_post() -> TestResult {
  let root = linux_root()?;
  let config = config_for(root.path());
  let mut job = captured_job()?;
  job.job_service_containers = Some(service_token()?);
  let workspace = config.workspace_root.join(&job.job_id);
  seed_probe(&workspace, "service-network", "action.yml")?;
  job.steps = vec![
    local_step(
      "service_action",
      "./.github/actions/service-network",
      &[
        ("marker", "SERVICE"),
        ("peer", "web"),
        ("peer_port", "80"),
        ("post_peer", "web"),
      ],
    ),
    ActionStep::script(
      "verify_service_action",
      r"
test '${{ steps.service_action.outputs.probe_output }}' = SERVICE-output
grep -q 'Welcome to nginx' docker-network.txt
printf service-host-verified > service-host-verified
",
      "",
    ),
  ];

  let events = collect_job(config, job, CancellationToken::new()).await?;
  assert_eq!(
    job_conclusion(&events),
    Some(Conclusion::Success),
    "{events:#?}"
  );
  assert_eq!(
    tokio::fs::read_to_string(workspace.join("service-host-verified")).await?,
    "service-host-verified"
  );
  assert_eq!(
    tokio::fs::read_to_string(workspace.join("docker-network-post.txt")).await?,
    "SERVICE-post-peer-ok"
  );
  let (service_id, service_network) = events
    .iter()
    .find_map(|event| {
      if let RunnerEvent::Log { line, .. } = event {
        line
          .strip_prefix("Service web created: ")
          .and_then(|resources| resources.split_once(" on "))
      } else {
        None
      }
    })
    .ok_or("service creation log absent")?;
  let docker = bollard::Docker::connect_with_local_defaults()?;
  assert!(is_missing(
    &docker
      .inspect_container(service_id, None::<InspectContainerOptions>)
      .await
  ));
  assert!(is_missing(
    &docker
      .inspect_network(service_network, None::<InspectNetworkOptions>)
      .await
  ));
  Ok(())
}

#[tokio::test]
#[ignore = "requires Linux, real Docker, and a daemon-shared test root"]
async fn entrypoint_failure_and_cancellation_fail_visibly_without_leaking_containers() -> TestResult
{
  let docker = bollard::Docker::connect_with_local_defaults()?;
  pull_alpine(&docker).await?;
  let sentinel = docker
    .create_container(
      None::<CreateContainerOptions>,
      ContainerCreateBody {
        image: Some(ALPINE.to_owned()),
        cmd: Some(vec!["sleep".to_owned(), "300".to_owned()]),
        ..Default::default()
      },
    )
    .await?;
  let scenario: TestResult = async {
    docker
      .start_container(&sentinel.id, None::<StartContainerOptions>)
      .await?;

    let failed_root = linux_root()?;
    let failed_config = config_for(failed_root.path());
    let mut failed_job = captured_job()?;
    let invalid_id = format!("invalid-entrypoint-{}", uuid::Uuid::new_v4().simple());
    failed_job.steps = vec![registry_step(&invalid_id, "/missing-entrypoint", "")];
    let failed_events = collect_job(failed_config, failed_job, CancellationToken::new()).await?;
    assert_eq!(
      job_conclusion(&failed_events),
      Some(Conclusion::Failure),
      "{failed_events:#?}"
    );
    assert!(
      action_container_ids(&docker, &invalid_id).await?.is_empty(),
      "invalid entrypoint left an owned container"
    );

    let main_fail_root = linux_root()?;
    let main_fail_config = config_for(main_fail_root.path());
    let mut main_fail_job = captured_job()?;
    let main_fail_workspace = main_fail_config.workspace_root.join(&main_fail_job.job_id);
    seed_probe(
      &main_fail_workspace,
      "main-failure",
      "action-failure-post.yml",
    )?;
    main_fail_job.steps = vec![local_step(
      "main_failure",
      "./.github/actions/main-failure",
      &[("marker", "MAINFAIL"), ("fail_main", "true")],
    )];
    let main_fail_events =
      collect_job(main_fail_config, main_fail_job, CancellationToken::new()).await?;
    assert_eq!(
      job_conclusion(&main_fail_events),
      Some(Conclusion::Failure),
      "{main_fail_events:#?}"
    );
    let stages = tokio::fs::read_to_string(main_fail_workspace.join("docker-stages.txt")).await?;
    let post = stages
      .lines()
      .find(|line| line.starts_with("MAINFAIL:post:"))
      .ok_or("failure() post marker absent")?;
    assert_eq!(
      post,
      "MAINFAIL:post:STATE_saved=MAINFAIL-state:STATE_pre_saved=<unset>"
    );

    let cancel_root = linux_root()?;
    let cancel_config = config_for(cancel_root.path());
    let mut cancel_job = captured_job()?;
    let workspace = cancel_config.workspace_root.join(&cancel_job.job_id);
    seed_probe(&workspace, "cancel", "action.yml")?;
    let cancel_step_id = format!("cancel-action-{}", uuid::Uuid::new_v4().simple());
    cancel_job.steps = vec![local_step(
      &cancel_step_id,
      "./.github/actions/cancel",
      &[("marker", "CANCEL"), ("sleep_main", "true")],
    )];
    let cancel = CancellationToken::new();
    let runner = Runner::new(cancel_config, Arc::new(Mutex::new(SecretMasker::new())));
    let redactor = MaskerRedactor(Arc::clone(runner.masker()));
    let mut receiver = runner.execute_job(cancel_job, cancel.clone());
    wait_for_file(&workspace.join("docker-cancel-started")).await?;
    cancel.cancel();
    let cancelled_events = tokio::time::timeout(Duration::from_secs(60), async {
      let mut events = Vec::new();
      while let Some(mut event) = receiver.recv().await {
        mask_event(&redactor, &mut event);
        events.push(event);
      }
      events
    })
    .await?;
    assert_eq!(
      job_conclusion(&cancelled_events),
      Some(Conclusion::Cancelled),
      "{cancelled_events:#?}"
    );
    let leaked = action_container_ids(&docker, &cancel_step_id).await?;
    assert!(
      leaked.is_empty(),
      "Docker action left containers: {leaked:?}"
    );
    let sentinel_state = docker
      .inspect_container(&sentinel.id, None::<InspectContainerOptions>)
      .await?
      .state
      .and_then(|state| state.running);
    assert_eq!(
      sentinel_state,
      Some(true),
      "action cleanup removed sentinel"
    );
    Ok(())
  }
  .await;
  let sentinel_cleanup = docker
    .remove_container(
      &sentinel.id,
      Some(RemoveContainerOptions {
        force: true,
        ..Default::default()
      }),
    )
    .await;
  scenario?;
  sentinel_cleanup?;
  Ok(())
}
