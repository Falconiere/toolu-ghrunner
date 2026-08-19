//! Reading this daemon's own containers back off Docker at startup — the
//! bollard half of `crate::adopt`.
//!
//! Two Docker calls, in this order: list every container carrying the
//! `sh.toolu.job-id` label (running or not), then inspect each one. The
//! inspect is not optional — a container's vCPU and memory limits are the
//! only record of what it consumes, and `ContainerSummary` does not carry
//! them. Reconstructing the footprint from `NanoCpus`/`Memory` is what makes
//! the adopted gate budget real rather than a guess.
//!
//! The deadline is read from the `sh.toolu.deadline` label and nowhere else.
//! `Config.Env` carries the same number in `TOOLU_DEADLINE`, but it also
//! carries `TOOLU_JITCONFIG` — a single-use GitHub credential — so this
//! module never reads that block at all. See `crate::docker::spec`.

use std::collections::HashMap;

use bollard::errors::Error as DockerError;
use bollard::models::{ContainerInspectResponse, ContainerStateStatusEnum};
use bollard::query_parameters::{InspectContainerOptions, ListContainersOptions};

use crate::adopt::{AdoptedContainer, ContainerLifecycle};
use crate::gate::JobSize;

use super::DockerBackend;
use super::spec::{JobLabels, LABEL_JOB_ID, memory_mb_from_bytes, vcpu_from_nano_cpus};

/// Where Docker's own status word places a container, in the three terms
/// adoption uses.
///
/// `paused` and `restarting` count as running: both still hold their cgroup
/// limits on the box, and a restarting container is on its way back rather
/// than done. `removing`, `dead` and the empty status count as finished —
/// there is no budget left to hold and nothing to restart. A container with
/// no status at all is finished too: an unreadable state is not grounds for
/// charging the gate.
pub fn lifecycle_of(status: Option<ContainerStateStatusEnum>) -> ContainerLifecycle {
  match status {
    Some(ContainerStateStatusEnum::CREATED) => ContainerLifecycle::Created,
    Some(
      ContainerStateStatusEnum::RUNNING
      | ContainerStateStatusEnum::PAUSED
      | ContainerStateStatusEnum::RESTARTING,
    ) => ContainerLifecycle::Running,
    Some(
      ContainerStateStatusEnum::EXITED
      | ContainerStateStatusEnum::DEAD
      | ContainerStateStatusEnum::REMOVING
      | ContainerStateStatusEnum::EMPTY,
    )
    | None => ContainerLifecycle::Finished,
  }
}

/// Turn one inspected container into an adoption input, or `None` when it
/// carries none of this daemon's labels — someone else's container on a
/// shared box, which holds no budget of ours and is not ours to remove.
///
/// The footprint comes from the limits `docker create` recorded; a container
/// of ours always has both, and one missing them reads as zero rather than
/// being invented.
pub fn adopted_from_inspect(
  container_id: &str,
  inspected: &ContainerInspectResponse,
) -> Option<AdoptedContainer> {
  let labels = inspected.config.as_ref().and_then(|config| {
    config
      .labels
      .as_ref()
      .and_then(|labels| JobLabels::from_map(labels).ok())
  })?;

  let host_config = inspected.host_config.as_ref();
  let size = JobSize {
    vcpu: vcpu_from_nano_cpus(host_config.and_then(|host| host.nano_cpus).unwrap_or(0)),
    memory_mb: memory_mb_from_bytes(host_config.and_then(|host| host.memory).unwrap_or(0)),
  };
  let lifecycle = lifecycle_of(inspected.state.as_ref().and_then(|state| state.status));

  Some(AdoptedContainer::new(
    labels.job_id,
    container_id,
    labels.deadline,
    size,
    lifecycle,
  ))
}

impl DockerBackend {
  /// Every job container this daemon left behind, as `crate::adopt` takes
  /// them: listed by label, then inspected one by one for the deadline and
  /// footprint adoption rebuilds its accounting from.
  ///
  /// A container that disappears between the list and its inspect is simply
  /// skipped — it is gone, which is the outcome adoption would have wanted
  /// for it anyway.
  ///
  /// # Errors
  ///
  /// Returns the bollard error if the listing itself fails. Startup treats
  /// that as fatal on purpose: a daemon that cannot read what is already
  /// running would seed an empty budget and then overcommit the box by every
  /// live container — the precise failure adoption exists to prevent.
  pub async fn existing_jobs(&self) -> Result<Vec<AdoptedContainer>, DockerError> {
    let filters = HashMap::from([("label".to_owned(), vec![LABEL_JOB_ID.to_owned()])]);
    let options = ListContainersOptions {
      all: true,
      filters: Some(filters),
      ..ListContainersOptions::default()
    };

    let listed = self.docker.list_containers(Some(options)).await?;
    let mut adopted = Vec::new();
    for container_id in listed.into_iter().filter_map(|container| container.id) {
      match self
        .docker
        .inspect_container(&container_id, None::<InspectContainerOptions>)
        .await
      {
        Ok(inspected) => adopted.extend(adopted_from_inspect(&container_id, &inspected)),
        Err(err) => {
          tracing::warn!(container_id, error = %err, "inspecting a container for adoption failed");
        },
      }
    }
    Ok(adopted)
  }

  /// Force-remove every container id in `container_ids`, best effort — what
  /// startup does with `crate::adopt::Adoption::remove` before it binds.
  /// Failures are logged where they happen; a container that survives is the
  /// deadline reaper's problem on the next tick, exactly as it is for every
  /// other removal path in this crate.
  pub async fn remove_all(&self, container_ids: &[String]) {
    for container_id in container_ids {
      self.force_remove(container_id).await;
    }
  }
}
