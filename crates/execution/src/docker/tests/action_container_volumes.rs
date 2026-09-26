//! Real-daemon anonymous-volume cleanup regression for Docker actions.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bollard::Docker;
use bollard::models::VolumeCreateRequest;
use bollard::query_parameters::{
  ListContainersOptionsBuilder, RemoveImageOptionsBuilder, RemoveVolumeOptions,
};
use shared::{Conclusion, RunnerConfig, SecretMasker};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::action_container::{ActionContainer, ActionContainerParams};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const IMAGE: &str =
  "alpine@sha256:ce64758a109eb420d874a118f87920e625e12d3634e03b4a5573fd9f6e5d3507";

fn daemon() -> Result<Docker, bollard::errors::Error> {
  let endpoint =
    crate::config::docker_host().unwrap_or_else(|| "unix:///var/run/docker.sock".to_owned());
  Docker::connect_with_host(&endpoint)
}

fn test_root() -> Result<tempfile::TempDir, std::io::Error> {
  let base = crate::config::var("TOOLU_CONTAINER_TEST_ROOT")
    .map(PathBuf::from)
    .ok_or_else(|| {
      std::io::Error::other("TOOLU_CONTAINER_TEST_ROOT is required for real-daemon tests")
    })?;
  std::fs::create_dir_all(&base)?;
  tempfile::Builder::new()
    .prefix("toolu action volume cleanup ")
    .tempdir_in(base)
}

fn config(root: &Path) -> RunnerConfig {
  RunnerConfig {
    data_dir: root.join("runner data"),
    workspace_root: root.join("work root"),
    ..RunnerConfig::default()
  }
}

async fn owned_volume(step_id: &str, destination: &str) -> TestResult<Option<String>> {
  let docker = daemon()?;
  let filters = HashMap::from([(
    "label".to_owned(),
    vec![format!("io.toolu.action-step={step_id}")],
  )]);
  let containers = docker
    .list_containers(Some(
      ListContainersOptionsBuilder::default()
        .all(true)
        .filters(&filters)
        .build(),
    ))
    .await?;
  let Some(id) = containers
    .first()
    .and_then(|container| container.id.as_deref())
  else {
    return Ok(None);
  };
  let inspected = docker.inspect_container(id, None).await?;
  Ok(
    inspected
      .mounts
      .unwrap_or_default()
      .into_iter()
      .find(|mount| mount.destination.as_deref() == Some(destination))
      .and_then(|mount| mount.name),
  )
}

async fn volume_exists(name: &str) -> TestResult<bool> {
  match daemon()?.inspect_volume(name).await {
    Ok(_) => Ok(true),
    Err(bollard::errors::Error::DockerResponseServerError {
      status_code: 404, ..
    }) => Ok(false),
    Err(error) => Err(error.into()),
  }
}

async fn remove_volume(name: &str) -> TestResult {
  daemon()?
    .remove_volume(name, Some(RemoveVolumeOptions { force: true }))
    .await?;
  Ok(())
}

async fn remove_image(image: &str) -> TestResult {
  daemon()?
    .remove_image(
      image,
      Some(RemoveImageOptionsBuilder::default().force(true).build()),
      None,
    )
    .await?;
  Ok(())
}

#[tokio::test]
#[ignore = "requires the authorized Linux Docker carrier and real local daemon"]
async fn cleanup_removes_owned_anonymous_volume_but_preserves_unrelated_named_volume() -> TestResult
{
  let root = test_root()?;
  let config = config(root.path());
  let workspace = config.workspace_root.join("job");
  let action_dir = root.path().join("action");
  tokio::fs::create_dir_all(&workspace).await?;
  tokio::fs::create_dir_all(&action_dir).await?;
  tokio::fs::write(
    action_dir.join("Dockerfile"),
    format!("FROM {IMAGE}\nVOLUME /action-owned\n"),
  )
  .await?;

  let runtime = ActionContainer::connect(Arc::new(Mutex::new(SecretMasker::new()))).await?;
  let cancel = CancellationToken::new();
  let image = runtime
    .prepare_image("Dockerfile", &action_dir, None, &cancel)
    .await?;
  let named = format!("toolu-unrelated-{}", uuid::Uuid::new_v4().simple());
  daemon()?
    .create_volume(VolumeCreateRequest {
      name: Some(named.clone()),
      ..Default::default()
    })
    .await?;

  let args = vec!["-ec".to_owned(), "sleep 60".to_owned()];
  let env = HashMap::new();
  let additions = Vec::new();
  let step_id = format!("volume-probe-{}", uuid::Uuid::new_v4().simple());
  let (events, _event_rx) = mpsc::channel(16);
  let (stdout, _stdout_rx) = mpsc::channel(16);
  let params = ActionContainerParams {
    image: &image,
    entrypoint: Some("/bin/sh"),
    args: Some(&args),
    env: &env,
    path_additions: &additions,
    config: &config,
    workspace: &workspace,
    action_dir: &action_dir,
    network: None,
    step_id: &step_id,
    timeout: Some(Duration::from_secs(30)),
    cancel: &cancel,
  };
  let capture_volume = async {
    for _attempt in 0..100 {
      if let Some(volume) = owned_volume(&step_id, "/action-owned").await? {
        cancel.cancel();
        return Ok::<String, Box<dyn std::error::Error>>(volume);
      }
      tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Err("action container anonymous volume was not observed".into())
  };
  let (result, owned) = tokio::join!(runtime.run(&params, &events, stdout), capture_volume);
  let conclusion = result?;
  let owned = owned?;
  let owned_remains = volume_exists(&owned).await?;
  let named_remains = volume_exists(&named).await?;
  let remove_owned = if owned_remains {
    remove_volume(&owned).await
  } else {
    Ok(())
  };
  let remove_named = remove_volume(&named).await;
  let remove_image = remove_image(&image).await;
  remove_owned?;
  remove_named?;
  remove_image?;

  assert_eq!(conclusion, Conclusion::Cancelled);
  assert!(!owned_remains, "owned anonymous volume {owned} leaked");
  assert!(named_remains, "unrelated named volume {named} was removed");
  assert_eq!(owned_containers(&step_id).await?, 0);
  Ok(())
}

async fn owned_containers(step_id: &str) -> TestResult<usize> {
  let filters = HashMap::from([(
    "label".to_owned(),
    vec![format!("io.toolu.action-step={step_id}")],
  )]);
  Ok(
    daemon()?
      .list_containers(Some(
        ListContainersOptionsBuilder::default()
          .all(true)
          .filters(&filters)
          .build(),
      ))
      .await?
      .len(),
  )
}
