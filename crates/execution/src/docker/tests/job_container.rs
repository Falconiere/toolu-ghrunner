//! Real Docker job-container lifecycle checks. No daemon or process doubles.

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::sync::{Arc, Mutex};

use futures_util::FutureExt;
use shared::{RunnerConfig, SecretMasker};
use tokio_util::sync::CancellationToken;

use super::JobContainer;
use crate::docker::container_spec::ContainerSpec;
use crate::execution::context::ExecutionContext;
use crate::execution::job_hooks::{JobHookStage, run_job_hook};

const IMAGE: &str =
  "ubuntu@sha256:008173c23f95b170204355c12626cb5a965d779a7e1283b09e9cffbb1bf33ca3";

#[tokio::test]
#[ignore = "requires a real local Linux Docker daemon and ubuntu:24.04"]
async fn job_container_real_mounts_env_metadata_and_cleanup()
-> Result<(), Box<dyn std::error::Error>> {
  // A VM-backed daemon needs an explicitly shared host directory.
  let base = std::env::var_os("TOOLU_CONTAINER_TEST_ROOT")
    .map_or_else(std::env::temp_dir, std::path::PathBuf::from);
  let root = tempfile::Builder::new()
    .prefix("toolu job container ")
    .tempdir_in(base)?;
  let config = RunnerConfig {
    data_dir: root.path().join("runner data"),
    workspace_root: root.path().join("work root"),
    ..RunnerConfig::default()
  };
  let workspace = config.workspace_root.join("job one");
  std::fs::create_dir_all(&workspace)?;
  std::fs::write(workspace.join("input file"), "real Docker mount\n")?;
  let spec = ContainerSpec {
    image: IMAGE.to_owned(),
    credentials: None,
    env: HashMap::from([("FROM_IMAGE_SETUP".to_owned(), "container value".to_owned())]),
    ports: Vec::new(),
    volumes: Vec::new(),
    options: vec!["--hostname".to_owned(), "toolu-job-probe".to_owned()],
  };
  let cancel = CancellationToken::new();
  let container = JobContainer::start(
    &spec,
    &config,
    &workspace,
    Arc::new(Mutex::new(SecretMasker::new())),
    &cancel,
  )
  .await?
  .ok_or("container setup unexpectedly cancelled")?;
  let result = AssertUnwindSafe(async {
    let opaque = format!("{}:data\nsecond line", workspace.join("opaque secret").display());
    let env = HashMap::from([
      ("GITHUB_OUTPUT".to_owned(), workspace.join("output file").display().to_string()),
      ("OPAQUE".to_owned(), opaque.clone()),
      ("DOCKER_HOST".to_owned(), "tcp://unreachable.invalid:1".to_owned()),
    ]);
    let (events, mut event_rx) = tokio::sync::mpsc::channel(64);
    let (stdout, mut stdout_rx) = tokio::sync::mpsc::channel(64);
    let args = ["-ec".to_owned(), "test \"$(hostname)\" = toolu-job-probe; test \"$FROM_IMAGE_SETUP\" = 'container value'; cat 'input file' > \"$GITHUB_OUTPUT\"; test -f /etc/os-release; printf '%s' \"$OPAQUE\" > opaque".to_owned()];
    let params = crate::docker::container_exec::ContainerExec {
      program: Path::new("sh"), args: &args, env: &env, working_dir: &workspace,
      step_id: "probe", timeout: Some(std::time::Duration::from_secs(30)), cancel: &cancel,
    };
    let result = container.execute(&params, &events, stdout).await?;
    drop(events);
    let mut diagnostics = Vec::new();
    while let Some(event) = event_rx.recv().await { diagnostics.push(format!("{event:?}")); }
    while let Some(line) = stdout_rx.recv().await { diagnostics.push(line); }
    assert_eq!(result, shared::Conclusion::Success, "{diagnostics:?}");
    assert_eq!(std::fs::read_to_string(workspace.join("output file"))?, "real Docker mount\n");
    assert_eq!(std::fs::read_to_string(workspace.join("opaque"))?, opaque);
    let inspect = tokio::process::Command::new("docker").args([
      "inspect", "--format", "{{.Id}} {{.HostConfig.NetworkMode}}", container.id(),
    ]).output().await?;
    assert!(inspect.status.success());
    assert_eq!(String::from_utf8(inspect.stdout)?.trim(), format!("{} {}", container.id(), container.network()));
    Ok::<(), Box<dyn std::error::Error>>(())
  }).catch_unwind().await;
  let cleanup = container.cleanup().await;
  result.map_err(|_panic| "container assertion failed; owned resources cleaned")??;
  cleanup?;
  let inspect = tokio::process::Command::new("docker")
    .args(["inspect", container.id()])
    .output()
    .await?;
  assert!(
    !inspect.status.success(),
    "owned container survived cleanup"
  );
  let network = tokio::process::Command::new("docker")
    .args(["network", "inspect", container.network()])
    .output()
    .await?;
  assert!(!network.status.success(), "owned network survived cleanup");
  Ok(())
}

#[tokio::test]
#[ignore = "requires a real local Linux Docker daemon and ubuntu:24.04"]
async fn job_container_real_host_hook_maps_github_path_back_to_host()
-> Result<(), Box<dyn std::error::Error>> {
  let base = std::env::var_os("TOOLU_CONTAINER_TEST_ROOT")
    .map_or_else(std::env::temp_dir, std::path::PathBuf::from);
  let root = tempfile::Builder::new()
    .prefix("toolu job container host hook ")
    .tempdir_in(base)?;
  let config = RunnerConfig {
    data_dir: root.path().join("runner data"),
    workspace_root: root.path().join("work root"),
    ..RunnerConfig::default()
  };
  let workspace = config.workspace_root.join("job one");
  std::fs::create_dir_all(&workspace)?;
  let masker = Arc::new(Mutex::new(SecretMasker::new()));
  let cancel = CancellationToken::new();
  let spec = ContainerSpec {
    image: IMAGE.to_owned(),
    credentials: None,
    env: HashMap::new(),
    ports: Vec::new(),
    volumes: Vec::new(),
    options: Vec::new(),
  };
  let container = Arc::new(
    JobContainer::start(&spec, &config, &workspace, Arc::clone(&masker), &cancel)
      .await?
      .ok_or_else(|| std::io::Error::other("container setup unexpectedly cancelled"))?,
  );
  let result = AssertUnwindSafe(async {
    let mut ctx = ExecutionContext::with_masker(masker);
    ctx.set_workspace(Some(workspace.clone()));
    ctx.set_runner_context("host-hook-probe", &config.data_dir)?;
    ctx.set_job_container(Arc::clone(&container));

    let runner_temp = config.data_dir.join("_temp");
    let env = HashMap::from([(
      "RUNNER_TEMP".to_owned(),
      runner_temp.to_string_lossy().into_owned(),
    )]);
    let args = vec![
      "-ec".to_owned(),
      "mkdir -p \"$RUNNER_TEMP/bin\"; printf '#!/bin/sh\\nprintf hook-helper-ran\\n' > \"$RUNNER_TEMP/bin/host-hook-helper\"; chmod +x \"$RUNNER_TEMP/bin/host-hook-helper\"".to_owned(),
    ];
    let (container_events, _container_events_rx) = tokio::sync::mpsc::channel(32);
    let (stdout, _stdout_rx) = tokio::sync::mpsc::channel(32);
    let conclusion = container
      .execute(
        &crate::docker::container_exec::ContainerExec {
          program: Path::new("sh"),
          args: &args,
          env: &env,
          working_dir: &workspace,
          step_id: "container-helper",
          timeout: Some(std::time::Duration::from_secs(30)),
          cancel: &cancel,
        },
        &container_events,
        stdout,
      )
      .await?;
    assert_eq!(conclusion, shared::Conclusion::Success);

    // File-command processing retains the path emitted by the container.
    // The hook is a host process, so its PATH must translate this one value
    // back to the mounted source while opaque values keep their exact bytes.
    ctx.prepend_path("/github/runner_temp/bin");
    ctx.set_env("OPAQUE_CONTAINER_VALUE", "/github/runner_temp/bin");
    let host_hook_marker = workspace.join("host-hook-marker");
    let hook = workspace.join("host-hook.sh");
    std::fs::write(
      &hook,
      "test \"$OPAQUE_CONTAINER_VALUE\" = /github/runner_temp/bin\nhost-hook-helper > \"$HOST_HOOK_MARKER\"\n",
    )?;
    ctx.set_env("HOST_HOOK_MARKER", host_hook_marker.to_string_lossy().as_ref());
    let host_env = ctx.build_host_step_env();
    assert!(host_env.get("PATH").is_some_and(|path| path.starts_with(runner_temp.join("bin").to_string_lossy().as_ref())), "host hook PATH must start with the mounted host directory");
    ctx.set_env(
      "ACTIONS_RUNNER_HOOK_JOB_COMPLETED",
      hook.to_string_lossy().as_ref(),
    );

    let (hook_events, _hook_events_rx) = tokio::sync::mpsc::channel(32);
    assert_eq!(
      run_job_hook(
        JobHookStage::Completed,
        &ctx,
        &hook_events,
        &workspace,
        &cancel,
      )
      .await?,
      Some(shared::Conclusion::Success)
    );
    assert_eq!(std::fs::read_to_string(host_hook_marker)?, "hook-helper-ran");
    Ok::<(), Box<dyn std::error::Error>>(())
  })
  .catch_unwind()
  .await;
  let cleanup = container.cleanup().await;
  result.map_err(|_panic| {
    std::io::Error::other("host-hook assertion failed; owned resources cleaned")
  })??;
  cleanup?;
  Ok(())
}
