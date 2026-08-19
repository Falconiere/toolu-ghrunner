//! Pulling and checking the pinned image — the bollard half of
//! `crate::prepull`.
//!
//! This is the *only* place in the crate that pulls. `crate::docker`'s create
//! path deliberately does not: a cold pull inside a request would blow the
//! client's ten-second timeout, which is terminal-with-cooldown on its side,
//! so a create against an absent image answers 503 and waits for the pull
//! that happens here.

use bollard::errors::Error as DockerError;
use bollard::query_parameters::CreateImageOptions;
use futures_util::StreamExt;

use crate::prepull::{ImagePresence, PullAttempt, PullOutcome, classify_pull, split_image_ref};

use super::{DockerBackend, is_not_found};

impl DockerBackend {
  /// Whether `image` is resident on the box right now.
  ///
  /// # Errors
  ///
  /// Returns the bollard error for anything other than Docker's "no such
  /// image" 404, which is the answer `Ok(false)` rather than a failure. A
  /// daemon that cannot be asked at all is a real fault and must not be read
  /// as "the image is missing".
  pub async fn image_present(&self, image: &str) -> Result<bool, DockerError> {
    match self.docker.inspect_image(image).await {
      Ok(_inspected) => Ok(true),
      Err(err) if is_not_found(&err) => Ok(false),
      Err(err) => Err(err),
    }
  }

  /// Pull `image`, draining the whole progress stream — the pull is only
  /// finished when the stream is.
  ///
  /// # Errors
  ///
  /// Returns the bollard error from the pull request or from any progress
  /// frame the daemon reported as an error.
  pub async fn pull_image(&self, image: &str) -> Result<(), DockerError> {
    let (repository, reference) = split_image_ref(image);
    let options = CreateImageOptions {
      from_image: Some(repository.to_owned()),
      tag: Some(reference.to_owned()),
      ..CreateImageOptions::default()
    };

    let mut stream = self.docker.create_image(Some(options), None, None);
    while let Some(frame) = stream.next().await {
      frame?;
    }
    Ok(())
  }

  /// One complete pre-pull attempt: pull `image`, then ask the box what it
  /// actually holds, and judge the two together with
  /// [`crate::prepull::classify_pull`].
  ///
  /// A pull failure is logged and folded into the outcome rather than
  /// returned, because the caller's decision does not turn on it: what
  /// matters is whether the image is there afterwards. A presence check that
  /// itself fails counts as absent — the daemon has no way to know the image
  /// is usable, and binding on an unverifiable image is what would answer
  /// 503 to real jobs.
  pub async fn attempt_pull(&self, image: &str) -> PullOutcome {
    let attempt = match self.pull_image(image).await {
      Ok(()) => PullAttempt::Succeeded,
      Err(err) => {
        tracing::warn!(image, error = %err, "pulling the pinned image failed");
        PullAttempt::Failed
      },
    };

    let presence = match self.image_present(image).await {
      Ok(true) => ImagePresence::Resident,
      Ok(false) => ImagePresence::Absent,
      Err(err) => {
        tracing::warn!(image, error = %err, "checking whether the pinned image is resident failed");
        ImagePresence::Absent
      },
    };

    classify_pull(attempt, presence)
  }
}
