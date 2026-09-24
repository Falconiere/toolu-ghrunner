//! Real Docker failure and cancellation coverage for owned job containers.

use std::collections::{BTreeSet, HashMap};
use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::FutureExt;
use shared::{Conclusion, RunnerConfig, SecretMasker};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::container_exec::ContainerExec;
use super::container_spec::ContainerSpec;
use super::job_container::JobContainer;

const IMAGE: &str =
  "ubuntu@sha256:008173c23f95b170204355c12626cb5a965d779a7e1283b09e9cffbb1bf33ca3";

#[tokio::test]
#[ignore = "requires a real local Linux Docker daemon and TOOLU_CONTAINER_TEST_ROOT"]
async fn cancellation_after_exec_starts_keeps_container_for_post_then_cleans_up()
-> Result<(), Box<dyn std::error::Error>> {
  let (root, config, workspace) = test_paths()?;
  let cancel = CancellationToken::new();
  let container = start_container(&config, &workspace, ContainerSpec::image_only(IMAGE)).await?;
  let result = AssertUnwindSafe(async {
    let started = workspace.join("cancellation-started");
    let execution = execute_script(
      &container,
      &workspace,
      &cancel,
      Some(Duration::from_secs(30)),
      "printf started > cancellation-started; sleep 30",
    );
    tokio::pin!(execution);
    let marker = wait_for_file(&started);
    tokio::pin!(marker);
    tokio::select! {
      conclusion = &mut execution => {
        return Err(format!("long-running exec completed before cancellation: {conclusion:?}").into());
      },
      marker = &mut marker => marker?,
    }
    cancel.cancel();
    assert_eq!(execution.await?, Conclusion::Cancelled);
    assert_eq!(
      execute_script(
        &container,
        &workspace,
        &CancellationToken::new(),
        Some(Duration::from_secs(10)),
        "printf post > cancellation-post",
      )
      .await?,
      Conclusion::Success
    );
    assert!(workspace.join("cancellation-post").is_file());
    Ok::<(), Box<dyn std::error::Error>>(())
  })
  .catch_unwind()
  .await;
  let cleanup = cleanup_and_verify(&container).await;
  drop(root);
  result.map_err(|_panic| "cancellation assertion failed; owned resources cleaned")??;
  cleanup
}

#[tokio::test]
#[ignore = "requires a real local Linux Docker daemon and TOOLU_CONTAINER_TEST_ROOT"]
async fn timeout_after_exec_starts_keeps_container_for_post_then_cleans_up()
-> Result<(), Box<dyn std::error::Error>> {
  let (root, config, workspace) = test_paths()?;
  let cancel = CancellationToken::new();
  let container = start_container(&config, &workspace, ContainerSpec::image_only(IMAGE)).await?;
  let result = AssertUnwindSafe(async {
    let conclusion = execute_script(
      &container,
      &workspace,
      &cancel,
      Some(Duration::from_millis(750)),
      "printf started > timeout-started; sleep 30",
    )
    .await?;
    assert!(workspace.join("timeout-started").is_file());
    assert_eq!(conclusion, Conclusion::Failure);
    assert_eq!(
      execute_script(
        &container,
        &workspace,
        &CancellationToken::new(),
        Some(Duration::from_secs(10)),
        "printf post > timeout-post",
      )
      .await?,
      Conclusion::Success
    );
    assert!(workspace.join("timeout-post").is_file());
    Ok::<(), Box<dyn std::error::Error>>(())
  })
  .catch_unwind()
  .await;
  let cleanup = cleanup_and_verify(&container).await;
  drop(root);
  result.map_err(|_panic| "timeout assertion failed; owned resources cleaned")??;
  cleanup
}

#[tokio::test]
#[ignore = "requires a real local Linux Docker daemon and TOOLU_CONTAINER_TEST_ROOT"]
async fn pull_create_and_start_failures_leave_no_owned_resources()
-> Result<(), Box<dyn std::error::Error>> {
  let (root, config, workspace) = test_paths()?;
  let before = owned_resources().await?;
  let pull_failure = ContainerSpec::image_only("127.0.0.1:1/toolu-missing-image:never");
  let pull = start_container(&config, &workspace, pull_failure).await;
  assert!(
    pull.is_err(),
    "an unreachable registry must fail image pull"
  );
  assert_eq!(
    owned_resources().await?,
    before,
    "pull failure left owned resources"
  );

  let create_failure = ContainerSpec {
    options: vec!["--memory".to_owned(), "1".to_owned()],
    ..ContainerSpec::image_only(IMAGE)
  };
  let create = start_container(&config, &workspace, create_failure).await;
  assert!(
    create.is_err(),
    "Docker must reject memory below its minimum"
  );
  assert_eq!(
    owned_resources().await?,
    before,
    "create failure left owned resources"
  );

  let start_failure = ContainerSpec {
    image: IMAGE.to_owned(),
    credentials: None,
    env: HashMap::new(),
    ports: Vec::new(),
    volumes: Vec::new(),
    options: vec!["--user".to_owned(), "toolu_missing_user_73".to_owned()],
  };
  let start = start_container(&config, &workspace, start_failure).await;
  assert!(start.is_err(), "a missing container user must fail startup");
  assert_eq!(
    owned_resources().await?,
    before,
    "start failure left owned resources"
  );
  drop(root);
  Ok(())
}

#[tokio::test]
#[ignore = "requires a real local Linux Docker daemon and TOOLU_CONTAINER_TEST_ROOT"]
async fn missing_exec_program_fails_and_owned_resources_are_removed()
-> Result<(), Box<dyn std::error::Error>> {
  let (_root, config, workspace) = test_paths()?;
  let container = start_container(&config, &workspace, ContainerSpec::image_only(IMAGE)).await?;
  let (events, _events_rx) = mpsc::channel(8);
  let (stdout, _stdout_rx) = mpsc::channel(8);
  let env = HashMap::new();
  let cancel = CancellationToken::new();
  let result = container
    .execute(
      &ContainerExec {
        program: Path::new("toolu_missing_executable_73"),
        args: &[],
        env: &env,
        working_dir: &workspace,
        step_id: "missing-program",
        timeout: Some(Duration::from_secs(10)),
        cancel: &cancel,
      },
      &events,
      stdout,
    )
    .await;
  let cleanup = cleanup_and_verify(&container).await;
  assert_eq!(
    result?,
    Conclusion::Failure,
    "missing executable must fail the step"
  );
  cleanup
}

fn test_paths()
-> Result<(tempfile::TempDir, RunnerConfig, std::path::PathBuf), Box<dyn std::error::Error>> {
  let base = std::env::var_os("TOOLU_CONTAINER_TEST_ROOT").ok_or_else(|| {
    std::io::Error::new(
      std::io::ErrorKind::NotFound,
      "TOOLU_CONTAINER_TEST_ROOT is required for Colima-shared real Docker tests",
    )
  })?;
  let root = tempfile::Builder::new()
    .prefix("toolu job container failure ")
    .tempdir_in(base)?;
  let config = RunnerConfig {
    data_dir: root.path().join("runner data"),
    workspace_root: root.path().join("work root"),
    ..RunnerConfig::default()
  };
  let workspace = config.workspace_root.join("job one");
  std::fs::create_dir_all(&workspace)?;
  Ok((root, config, workspace))
}

async fn start_container(
  config: &RunnerConfig,
  workspace: &Path,
  spec: ContainerSpec,
) -> Result<JobContainer, Box<dyn std::error::Error>> {
  Ok(
    JobContainer::start(
      &spec,
      config,
      workspace,
      Arc::new(Mutex::new(SecretMasker::new())),
      &CancellationToken::new(),
    )
    .await?
    .ok_or("container setup unexpectedly cancelled")?,
  )
}

async fn execute_script(
  container: &JobContainer,
  workspace: &Path,
  cancel: &CancellationToken,
  timeout: Option<Duration>,
  script: &str,
) -> Result<Conclusion, Box<dyn std::error::Error>> {
  let args = vec!["-ec".to_owned(), script.to_owned()];
  let env = HashMap::new();
  let (events, _events_rx) = mpsc::channel(8);
  let (stdout, _stdout_rx) = mpsc::channel(8);
  Ok(
    container
      .execute(
        &ContainerExec {
          program: Path::new("sh"),
          args: &args,
          env: &env,
          working_dir: workspace,
          step_id: "real-failure-probe",
          timeout,
          cancel,
        },
        &events,
        stdout,
      )
      .await?,
  )
}

async fn wait_for_file(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
  tokio::time::timeout(Duration::from_secs(10), async {
    while !path.is_file() {
      tokio::time::sleep(Duration::from_millis(25)).await;
    }
  })
  .await
  .map_err(|_timeout| format!("timed out waiting for {}", path.display()))?;
  Ok(())
}

async fn cleanup_and_verify(container: &JobContainer) -> Result<(), Box<dyn std::error::Error>> {
  let id = container.id().to_owned();
  let network = container.network().to_owned();
  container.cleanup().await?;
  assert!(!docker_exists(["inspect", &id]).await?);
  assert!(!docker_exists(["network", "inspect", &network]).await?);
  Ok(())
}

async fn owned_resources()
-> Result<(BTreeSet<String>, BTreeSet<String>), Box<dyn std::error::Error>> {
  Ok((
    docker_lines(["ps", "-aq", "--filter", "label=io.toolu.job-container=true"]).await?,
    docker_lines([
      "network",
      "ls",
      "-q",
      "--filter",
      "label=io.toolu.job-container=true",
    ])
    .await?,
  ))
}

async fn docker_exists<const N: usize>(
  args: [&str; N],
) -> Result<bool, Box<dyn std::error::Error>> {
  let output = tokio::process::Command::new("docker")
    .args(args)
    .output()
    .await?;
  Ok(output.status.success())
}

async fn docker_lines<const N: usize>(
  args: [&str; N],
) -> Result<BTreeSet<String>, Box<dyn std::error::Error>> {
  let output = tokio::process::Command::new("docker")
    .args(args)
    .output()
    .await?;
  if !output.status.success() {
    return Err(
      format!(
        "docker {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
      )
      .into(),
    );
  }
  Ok(
    String::from_utf8(output.stdout)?
      .lines()
      .map(ToOwned::to_owned)
      .collect(),
  )
}

impl ContainerSpec {
  fn image_only(image: &str) -> Self {
    Self {
      image: image.to_owned(),
      credentials: None,
      env: HashMap::new(),
      ports: Vec::new(),
      volumes: Vec::new(),
      options: Vec::new(),
    }
  }
}

#[cfg(unix)]
#[test]
fn job_container_missing_daemon_reports_connection_failure()
-> Result<(), Box<dyn std::error::Error>> {
  let root = tempfile::tempdir()?;
  let endpoint = format!(
    "unix://{}",
    root.path().join("missing-docker.sock").display()
  );
  let runtime = tokio::runtime::Runtime::new()?;
  temp_env::with_var("DOCKER_HOST", Some(endpoint), || {
    let result = runtime.block_on(super::container_command::ContainerCommand::connect(
      Arc::new(Mutex::new(SecretMasker::new())),
    ));
    let Err(error) = result else {
      return Err("nonexistent Docker socket unexpectedly connected".into());
    };
    assert!(
      error
        .to_string()
        .starts_with("docker error: connect local Docker daemon:"),
      "{error}"
    );
    assert!(error.to_string().contains("missing-docker.sock"), "{error}");
    Ok(())
  })
}
