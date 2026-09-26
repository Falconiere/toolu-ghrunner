//! Registry pull and Dockerfile build preparation for Docker actions.

use std::path::{Component, Path};

use bollard::body_full;
use bollard::query_parameters::{BuildImageOptionsBuilder, CreateImageOptionsBuilder};
use futures_util::StreamExt;
use shared::RunnerError;
use tokio_util::sync::CancellationToken;

use super::action_archive::archive_context;
use super::action_container::ActionContainer;

impl ActionContainer {
  /// Pull a registry image or build an action-local Dockerfile.
  pub(crate) async fn prepare_image(
    &self,
    image: &str,
    action_dir: &Path,
    cache_key: Option<&str>,
    cancel: &CancellationToken,
  ) -> Result<String, RunnerError> {
    check_cancel(cancel)?;
    if let Some(registry) = image.strip_prefix("docker://") {
      return self.pull_image(registry, cancel).await;
    }
    self.build_image(image, action_dir, cache_key, cancel).await
  }

  async fn pull_image(
    &self,
    image: &str,
    cancel: &CancellationToken,
  ) -> Result<String, RunnerError> {
    if image.trim().is_empty() {
      return Err(RunnerError::Docker(
        "Docker action registry image is empty".to_owned(),
      ));
    }
    let options = CreateImageOptionsBuilder::default()
      .from_image(image)
      .build();
    let mut stream = self
      .transport
      .docker
      .create_image(Some(options), None, None);
    loop {
      let next = tokio::select! {
        () = cancel.cancelled() => return Err(RunnerError::Cancelled),
        next = stream.next() => next,
      };
      let Some(next) = next else { break };
      let info = next.map_err(|error| self.transport.error("pull Docker action image", error))?;
      if let Some(message) = info.error_detail.and_then(|detail| detail.message) {
        return Err(self.transport.error("pull Docker action image", message));
      }
    }
    self
      .transport
      .docker
      .inspect_image(image)
      .await
      .map_err(|error| {
        self
          .transport
          .error("inspect pulled Docker action image", error)
      })?;
    Ok(image.to_owned())
  }

  async fn build_image(
    &self,
    dockerfile: &str,
    action_dir: &Path,
    cache_key: Option<&str>,
    cancel: &CancellationToken,
  ) -> Result<String, RunnerError> {
    let (context_dir, dockerfile_name) =
      resolve_dockerfile(dockerfile.to_owned(), action_dir.to_path_buf()).await?;
    let tag = self.build_tag(cache_key, dockerfile).await?;
    if cache_key.is_some() && self.image_exists(&tag).await? {
      return Ok(tag);
    }
    let archive = archive_action_context(context_dir, dockerfile_name.clone()).await?;
    check_cancel(cancel)?;
    let options = BuildImageOptionsBuilder::default()
      .dockerfile(&dockerfile_name)
      .t(&tag)
      .rm(true)
      .forcerm(true)
      .build();
    let mut stream =
      self
        .transport
        .docker
        .build_image(options, None, Some(body_full(archive.into())));
    loop {
      let next = tokio::select! {
        () = cancel.cancelled() => return Err(RunnerError::Cancelled),
        next = stream.next() => next,
      };
      let Some(next) = next else { break };
      let info = next.map_err(|error| self.transport.error("build Docker action image", error))?;
      if let Some(message) = info.error_detail.and_then(|detail| detail.message) {
        return Err(self.transport.error("build Docker action image", message));
      }
    }
    self
      .transport
      .docker
      .inspect_image(&tag)
      .await
      .map_err(|error| {
        self
          .transport
          .error("inspect built Docker action image", error)
      })?;
    Ok(tag)
  }

  async fn image_exists(&self, image: &str) -> Result<bool, RunnerError> {
    match self.transport.docker.inspect_image(image).await {
      Ok(_) => Ok(true),
      Err(bollard::errors::Error::DockerResponseServerError {
        status_code: 404, ..
      }) => Ok(false),
      Err(error) => Err(
        self
          .transport
          .error("inspect cached Docker action image", error),
      ),
    }
  }

  async fn build_tag(
    &self,
    cache_key: Option<&str>,
    dockerfile: &str,
  ) -> Result<String, RunnerError> {
    let Some(cache_key) = cache_key else {
      return Ok(format!(
        "toolu-action:local-{}",
        uuid::Uuid::new_v4().simple()
      ));
    };
    let version = self.transport.docker.version().await.map_err(|error| {
      self
        .transport
        .error("inspect Docker action platform", error)
    })?;
    let identity = format!(
      "{cache_key}\n{dockerfile}\n{}/{}",
      version.os.unwrap_or_else(|| "unknown".to_owned()),
      version.arch.unwrap_or_else(|| "unknown".to_owned())
    );
    Ok(format!(
      "toolu-action:{}",
      blake3::hash(identity.as_bytes()).to_hex()
    ))
  }
}

async fn archive_action_context(
  context_dir: std::path::PathBuf,
  dockerfile: String,
) -> Result<Vec<u8>, RunnerError> {
  tokio::task::spawn_blocking(move || archive_context(&context_dir, &dockerfile))
    .await
    .map_err(|error| RunnerError::Docker(format!("archive Docker action context: {error}")))?
    .map_err(RunnerError::from)
}

async fn resolve_dockerfile(
  dockerfile: String,
  action_dir: std::path::PathBuf,
) -> Result<(std::path::PathBuf, String), RunnerError> {
  tokio::task::spawn_blocking(move || resolve_dockerfile_sync(&dockerfile, &action_dir))
    .await
    .map_err(|error| RunnerError::Docker(format!("resolve Docker action context: {error}")))?
}

fn resolve_dockerfile_sync(
  dockerfile: &str,
  action_dir: &Path,
) -> Result<(std::path::PathBuf, String), RunnerError> {
  let path = Path::new(dockerfile);
  if dockerfile.is_empty()
    || path.is_absolute()
    || path
      .components()
      .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
  {
    return Err(RunnerError::Docker(format!(
      "Docker action Dockerfile path is invalid: {dockerfile}"
    )));
  }
  let full = action_dir.join(path);
  if !full.is_file() {
    return Err(RunnerError::Docker(format!(
      "Docker action Dockerfile does not exist: {}",
      full.display()
    )));
  }
  let context = full.parent().ok_or_else(|| {
    RunnerError::Docker(format!(
      "Docker action Dockerfile has no parent: {}",
      full.display()
    ))
  })?;
  let name = full
    .file_name()
    .and_then(std::ffi::OsStr::to_str)
    .ok_or_else(|| RunnerError::Docker("Dockerfile name is not valid Unicode".to_owned()))?;
  Ok((context.to_path_buf(), name.to_owned()))
}

fn check_cancel(cancel: &CancellationToken) -> Result<(), RunnerError> {
  if cancel.is_cancelled() {
    Err(RunnerError::Cancelled)
  } else {
    Ok(())
  }
}
