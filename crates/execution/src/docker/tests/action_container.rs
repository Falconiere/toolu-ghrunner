//! Real-daemon regression tests for owned Docker action containers.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bollard::Docker;
use bollard::query_parameters::{ListContainersOptionsBuilder, RemoveImageOptionsBuilder};
use shared::{Conclusion, LogStream, RunnerConfig, RunnerEvent, SecretMasker};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::action_container::{ActionContainer, ActionContainerParams};
use super::action_mounts::action_path_to_host;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const IMAGE: &str =
  "alpine@sha256:ce64758a109eb420d874a118f87920e625e12d3634e03b4a5573fd9f6e5d3507";

fn daemon() -> Result<Docker, bollard::errors::Error> {
  let endpoint =
    crate::config::docker_host().unwrap_or_else(|| "unix:///var/run/docker.sock".to_owned());
  Docker::connect_with_host(&endpoint)
}

fn test_root(prefix: &str) -> Result<tempfile::TempDir, std::io::Error> {
  let base =
    crate::config::var("TOOLU_CONTAINER_TEST_ROOT").map_or_else(std::env::temp_dir, PathBuf::from);
  std::fs::create_dir_all(&base)?;
  tempfile::Builder::new().prefix(prefix).tempdir_in(base)
}

fn config(root: &Path) -> RunnerConfig {
  RunnerConfig {
    data_dir: root.join("runner data"),
    workspace_root: root.join("work root"),
    ..RunnerConfig::default()
  }
}

async fn remove_image(image: &str) {
  let Ok(docker) = daemon() else { return };
  let _result = docker
    .remove_image(
      image,
      Some(RemoveImageOptionsBuilder::default().force(true).build()),
      None,
    )
    .await;
}

async fn image_id(image: &str) -> TestResult<String> {
  daemon()?
    .inspect_image(image)
    .await?
    .id
    .ok_or_else(|| "prepared image has no id".into())
}

async fn owned_containers(step_id: &str) -> TestResult<usize> {
  let filters = HashMap::from([(
    "label".to_owned(),
    vec![format!("io.toolu.action-step={step_id}")],
  )]);
  let containers = daemon()?
    .list_containers(Some(
      ListContainersOptionsBuilder::default()
        .all(true)
        .filters(&filters)
        .build(),
    ))
    .await?;
  Ok(containers.len())
}

#[test]
fn action_path_mapping_returns_host_coordinates_without_io() {
  let config = RunnerConfig {
    data_dir: PathBuf::from("/runner data"),
    workspace_root: PathBuf::from("/work"),
    ..RunnerConfig::default()
  };
  assert_eq!(
    action_path_to_host(
      &config,
      Path::new("/work/job one"),
      Path::new("/runner data/actions/owner/repo"),
      "/github/workspace/bin with spaces",
    ),
    PathBuf::from("/work/job one/bin with spaces")
  );
  assert_eq!(
    action_path_to_host(
      &config,
      Path::new("/work/job one"),
      Path::new("/runner data/actions/owner/repo"),
      "/github/runner_temp/tool/bin",
    ),
    PathBuf::from("/runner data/_temp/tool/bin")
  );
  assert_eq!(
    action_path_to_host(
      &config,
      Path::new("/work/job one"),
      Path::new("/work/job one"),
      "/github/workspace/bin",
    ),
    PathBuf::from("/work/job one/bin")
  );
}

#[tokio::test]
#[ignore = "requires the authorized Linux Docker carrier and real local daemon"]
async fn prepares_registry_and_reuses_only_keyed_dockerfile_builds() -> TestResult {
  let root = test_root("toolu action image ")?;
  let action_dir = root.path().join("action");
  tokio::fs::create_dir_all(&action_dir).await?;
  let runtime = ActionContainer::connect(Arc::new(Mutex::new(SecretMasker::new()))).await?;
  let cancel = CancellationToken::new();

  let registry = runtime
    .prepare_image(&format!("docker://{IMAGE}"), &action_dir, None, &cancel)
    .await?;
  assert_eq!(registry, IMAGE);
  assert!(!image_id(&registry).await?.is_empty());

  let nested = action_dir.join("nested");
  tokio::fs::create_dir_all(&nested).await?;
  tokio::fs::write(nested.join("sibling"), "nested-context\n").await?;
  tokio::fs::write(
    nested.join("Dockerfile"),
    format!("FROM {IMAGE}\nCOPY sibling /nested-context\n"),
  )
  .await?;
  let nested_image = runtime
    .prepare_image(
      "nested/Dockerfile",
      &action_dir,
      Some("github.example/Owner/Repo@0123456789abcdef"),
      &cancel,
    )
    .await?;
  assert!(!image_id(&nested_image).await?.is_empty());

  tokio::fs::write(
    action_dir.join("Dockerfile"),
    format!("FROM {IMAGE}\nLABEL io.toolu.probe=first\n"),
  )
  .await?;
  let cached = runtime
    .prepare_image(
      "Dockerfile",
      &action_dir,
      Some("github.example/Owner/Repo@0123456789abcdef:path:linux-arm64"),
      &cancel,
    )
    .await?;
  let first_id = image_id(&cached).await?;

  tokio::fs::write(
    action_dir.join("Dockerfile"),
    format!("FROM {IMAGE}\nLABEL io.toolu.probe=changed\n"),
  )
  .await?;
  let cached_again = runtime
    .prepare_image(
      "Dockerfile",
      &action_dir,
      Some("github.example/Owner/Repo@0123456789abcdef:path:linux-arm64"),
      &cancel,
    )
    .await?;
  assert_eq!(cached_again, cached);
  assert_eq!(image_id(&cached_again).await?, first_id);

  let local = runtime
    .prepare_image("Dockerfile", &action_dir, None, &cancel)
    .await?;
  assert_ne!(local, cached);
  assert_ne!(image_id(&local).await?, first_id);

  let missing = runtime
    .prepare_image("missing.Dockerfile", &action_dir, None, &cancel)
    .await
    .expect_err("a missing Dockerfile must fail preparation");
  assert!(missing.to_string().contains("missing.Dockerfile"));

  tokio::fs::write(
    action_dir.join("broken.Dockerfile"),
    format!("FROM {IMAGE}\nRUN /definitely-missing-toolu-command\n"),
  )
  .await?;
  let broken = runtime
    .prepare_image("broken.Dockerfile", &action_dir, None, &cancel)
    .await
    .expect_err("a failing Dockerfile instruction must fail preparation");
  assert!(broken.to_string().contains("build Docker action image"));

  let missing_registry = runtime
    .prepare_image(
      "docker://localhost:1/toolu-action-missing:never",
      &action_dir,
      None,
      &cancel,
    )
    .await
    .expect_err("a nonexistent registry image must fail preparation");
  assert!(
    missing_registry
      .to_string()
      .contains("pull Docker action image")
  );

  remove_image(&local).await;
  remove_image(&cached).await;
  remove_image(&nested_image).await;
  Ok(())
}

#[tokio::test]
#[ignore = "requires the authorized Linux Docker carrier and real local daemon"]
async fn action_run_preserves_image_path_mounts_streams_and_cleans_up() -> TestResult {
  let root = test_root("toolu action run ")?;
  let config = config(root.path());
  let workspace = config.workspace_root.join("job with spaces");
  let action_dir = root.path().join("action with spaces");
  tokio::fs::create_dir_all(&workspace).await?;
  tokio::fs::create_dir_all(&action_dir).await?;
  tokio::fs::write(workspace.join("input file"), "workspace-value\n").await?;
  tokio::fs::write(action_dir.join("action file"), "action-value\n").await?;
  let events_dir = config.data_dir.join("events");
  tokio::fs::create_dir_all(&events_dir).await?;
  tokio::fs::write(events_dir.join("event.json"), "{}\n").await?;
  let addition = config.data_dir.join("_temp/tool bin");
  tokio::fs::create_dir_all(&addition).await?;

  let runtime = ActionContainer::connect(Arc::new(Mutex::new(SecretMasker::new()))).await?;
  let cancel = CancellationToken::new();
  let image = runtime
    .prepare_image(&format!("docker://{IMAGE}"), &action_dir, None, &cancel)
    .await?;
  let output = workspace.join("command output");
  let env = HashMap::from([
    (
      "GITHUB_OUTPUT".to_owned(),
      output.to_string_lossy().into_owned(),
    ),
    (
      "GITHUB_EVENT_PATH".to_owned(),
      events_dir.join("event.json").to_string_lossy().into_owned(),
    ),
    ("EMPTY_VALUE".to_owned(), String::new()),
    ("SPACE_VALUE".to_owned(), "two words".to_owned()),
    (
      "CUSTOM_WORKSPACE".to_owned(),
      workspace.to_string_lossy().into_owned(),
    ),
  ]);
  let args = vec![
    "-ec".to_owned(),
    concat!(
      "test \"$PATH\" = '/github/runner_temp/tool bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin'; ",
      "test \"$HOME\" = /github/home; ",
      "test \"$CUSTOM_WORKSPACE\" = /github/workspace; ",
      "test \"$EMPTY_VALUE\" = ''; test \"$SPACE_VALUE\" = 'two words'; ",
      "test \"$(cat 'input file')\" = workspace-value; ",
      "test \"$(cat /github/action/'action file')\" = action-value; ",
      "test -f \"$GITHUB_EVENT_PATH\"; test -d /github/file_commands; ",
      "test -d /github/runner_temp; test -d /github/tool_cache; ",
      "test -S /var/run/docker.sock; ",
      "printf shared > \"$HOME/shared\"; ",
      "printf 'answer=space value\\n' > \"$GITHUB_OUTPUT\"; ",
      "printf 'stdout line\\n'; printf 'stderr line\\n' >&2; ",
      "printf '::warning::stderr command\\n' >&2"
    )
    .to_owned(),
  ];
  let additions = vec![addition.to_string_lossy().into_owned()];
  let step_id = format!("mount-probe-{}", uuid::Uuid::new_v4().simple());
  let (events, mut event_rx) = mpsc::channel(16);
  let (stdout, mut stdout_rx) = mpsc::channel(16);
  let conclusion = runtime
    .run(
      &ActionContainerParams {
        image: &image,
        entrypoint: Some("/bin/sh"),
        args: Some(&args),
        env: &env,
        config: &config,
        workspace: &workspace,
        action_dir: &action_dir,
        network: None,
        step_id: &step_id,
        path_additions: &additions,
        timeout: Some(Duration::from_secs(30)),
        cancel: &cancel,
      },
      &events,
      stdout,
    )
    .await?;
  drop(events);
  assert_eq!(conclusion, Conclusion::Success);
  // Docker multiplexes the two file descriptors without a shared line order.
  let mut dispatched = [stdout_rx.recv().await, stdout_rx.recv().await];
  dispatched.sort();
  assert_eq!(
    dispatched,
    [
      Some("::warning::stderr command".to_owned()),
      Some("stdout line".to_owned())
    ]
  );
  assert!(matches!(
    event_rx.recv().await,
    Some(RunnerEvent::Log { line, stream: LogStream::Stderr, .. })
      if line == "stderr line"
  ));
  assert_eq!(
    tokio::fs::read_to_string(output).await?,
    "answer=space value\n"
  );
  assert_eq!(owned_containers(&step_id).await?, 0);

  let second_args = vec!["-ec".to_owned(), "test -f \"$HOME/shared\"".to_owned()];
  let second_step_id = format!("home-probe-{}", uuid::Uuid::new_v4().simple());
  let (second_events, _second_event_rx) = mpsc::channel(16);
  let (second_stdout, _second_stdout_rx) = mpsc::channel(16);
  let second = runtime
    .run(
      &ActionContainerParams {
        image: &image,
        entrypoint: Some("/bin/sh"),
        args: Some(&second_args),
        env: &HashMap::new(),
        config: &config,
        workspace: &workspace,
        action_dir: &action_dir,
        network: None,
        step_id: &second_step_id,
        path_additions: &[],
        timeout: Some(Duration::from_secs(30)),
        cancel: &cancel,
      },
      &second_events,
      second_stdout,
    )
    .await?;
  assert_eq!(second, Conclusion::Success);
  assert_eq!(owned_containers(&second_step_id).await?, 0);
  Ok(())
}

#[tokio::test]
#[ignore = "requires the authorized Linux Docker carrier and real local daemon"]
async fn cancellation_after_start_returns_cancelled_and_removes_owned_container() -> TestResult {
  let root = test_root("toolu action cancel ")?;
  let config = config(root.path());
  let workspace = config.workspace_root.join("job");
  let action_dir = root.path().join("action");
  tokio::fs::create_dir_all(&workspace).await?;
  tokio::fs::create_dir_all(&action_dir).await?;
  let runtime = ActionContainer::connect(Arc::new(Mutex::new(SecretMasker::new()))).await?;
  let cancel = CancellationToken::new();
  let image = runtime
    .prepare_image(&format!("docker://{IMAGE}"), &action_dir, None, &cancel)
    .await?;
  let args = vec![
    "-ec".to_owned(),
    "printf started > started; sleep 60".to_owned(),
  ];
  let env = HashMap::new();
  let additions = Vec::new();
  let step_id = format!("cancel-probe-{}", uuid::Uuid::new_v4().simple());
  let (events, _event_rx) = mpsc::channel(16);
  let (stdout, _stdout_rx) = mpsc::channel(16);
  let params = ActionContainerParams {
    image: &image,
    entrypoint: Some("/bin/sh"),
    args: Some(&args),
    env: &env,
    config: &config,
    workspace: &workspace,
    action_dir: &action_dir,
    network: None,
    step_id: &step_id,
    path_additions: &additions,
    timeout: Some(Duration::from_secs(30)),
    cancel: &cancel,
  };
  let cancel_after_start = async {
    for _attempt in 0..100 {
      if workspace.join("started").exists() {
        cancel.cancel();
        return Ok::<(), std::io::Error>(());
      }
      tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Err(std::io::Error::other("action container did not start"))
  };
  let (result, cancelled) = tokio::join!(runtime.run(&params, &events, stdout), cancel_after_start);
  cancelled?;
  assert_eq!(result?, Conclusion::Cancelled);
  assert_eq!(owned_containers(&step_id).await?, 0);
  Ok(())
}

#[tokio::test]
#[ignore = "requires the authorized Linux Docker carrier and real local daemon"]
async fn timeout_returns_failure_and_removes_owned_container() -> TestResult {
  let root = test_root("toolu action timeout ")?;
  let config = config(root.path());
  let workspace = config.workspace_root.join("job");
  let action_dir = root.path().join("action");
  tokio::fs::create_dir_all(&workspace).await?;
  tokio::fs::create_dir_all(&action_dir).await?;
  let runtime = ActionContainer::connect(Arc::new(Mutex::new(SecretMasker::new()))).await?;
  let cancel = CancellationToken::new();
  let image = runtime
    .prepare_image(&format!("docker://{IMAGE}"), &action_dir, None, &cancel)
    .await?;
  let args = vec!["-ec".to_owned(), "sleep 60".to_owned()];
  let env = HashMap::new();
  let additions = Vec::new();
  let step_id = format!("timeout-probe-{}", uuid::Uuid::new_v4().simple());
  let (events, mut event_rx) = mpsc::channel(16);
  let (stdout, _stdout_rx) = mpsc::channel(16);
  let conclusion = runtime
    .run(
      &ActionContainerParams {
        image: &image,
        entrypoint: Some("/bin/sh"),
        args: Some(&args),
        env: &env,
        config: &config,
        workspace: &workspace,
        action_dir: &action_dir,
        network: None,
        step_id: &step_id,
        path_additions: &additions,
        timeout: Some(Duration::from_millis(200)),
        cancel: &cancel,
      },
      &events,
      stdout,
    )
    .await?;
  assert_eq!(conclusion, Conclusion::Failure);
  assert!(matches!(
    event_rx.recv().await,
    Some(RunnerEvent::Log { line, stream: LogStream::Stderr, .. })
      if line.contains("timed out")
  ));
  assert_eq!(owned_containers(&step_id).await?, 0);
  Ok(())
}
