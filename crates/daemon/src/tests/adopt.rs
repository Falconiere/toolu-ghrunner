//! Tests for startup adoption's decision core: given the job containers a
//! restarted daemon finds on the box, what budget it rebuilds, which
//! deadlines it re-arms, and what it removes instead.
//!
//! Real data throughout. The footprints are rows of
//! `packages/api/src/github/runner-tags.ts` (toolu.sh repo); the labels and
//! Docker limits are the ones `crate::docker::spec::container_config`
//! actually sends, read back through the same inspect shape bollard returns,
//! so the round trip a restart depends on is asserted end to end rather than
//! assumed.
//!
//! Nothing here connects to Docker: CI runs `cargo test --workspace` on
//! `macos-14`, which has no Docker daemon at all. The bollard model values
//! below are plain data, and the live behaviour is `tests/docker_live.rs`.

use std::collections::HashMap;

use bollard::models::{
  ContainerConfig, ContainerInspectResponse, ContainerState, ContainerStateStatusEnum,
};

use super::{AdoptedContainer, AdoptionAction, ContainerLifecycle, action_for, adopt};
use crate::docker::inventory::{adopted_from_inspect, lifecycle_of};
use crate::docker::spec::{ENV_DEADLINE, ENV_JIT_CONFIG, container_config};
use crate::gate::{Gate, JobId, JobSize, StartOutcome};
use crate::reaper::{CreatedContainers, StartQueue};
use crate::routes::wire::{CreateJobRequest, JobRefWire, JobSizeWire};

/// `toolu-ubuntu` / `toolu-ubuntu-2vcpu-4gb` — the Linux floor.
const TAG_2VCPU_4GB: JobSize = JobSize {
  vcpu: 2,
  memory_mb: 4096,
};
/// `toolu-ubuntu-8vcpu-16gb`.
const TAG_8VCPU_16GB: JobSize = JobSize {
  vcpu: 8,
  memory_mb: 16384,
};
/// `toolu-ubuntu-16vcpu-32gb` — the largest Linux tag toolu.sh can send, used
/// here as the whole box's budget.
const TAG_16VCPU_32GB: JobSize = JobSize {
  vcpu: 16,
  memory_mb: 32768,
};

/// Every Linux tag `runner-tags.ts` sells, plus the widest macOS shape.
const CATALOG: [JobSize; 6] = [
  JobSize {
    vcpu: 2,
    memory_mb: 4096,
  },
  JobSize {
    vcpu: 4,
    memory_mb: 8192,
  },
  JobSize {
    vcpu: 8,
    memory_mb: 16384,
  },
  JobSize {
    vcpu: 8,
    memory_mb: 32768,
  },
  JobSize {
    vcpu: 16,
    memory_mb: 32768,
  },
  JobSize {
    vcpu: 12,
    memory_mb: 57344,
  },
];

/// A real epoch-millisecond deadline: `nowMs` plus six hours, what
/// `client.ts` sends.
const DEADLINE_MS: i64 = 1_763_000_000_000;
/// A restart happening one hour before that deadline.
const RESTART_MS: i64 = DEADLINE_MS - 60 * 60 * 1000;

/// A container adoption should find, with everything but the varying parts
/// fixed.
fn container(
  job_id: &str,
  size: JobSize,
  lifecycle: ContainerLifecycle,
  deadline_ms: i64,
) -> AdoptedContainer {
  AdoptedContainer::new(
    job_id,
    format!("container-for-{job_id}"),
    deadline_ms,
    size,
    lifecycle,
  )
}

/// A fresh, empty process: the gate, queue and created-container map a
/// restarted daemon starts with before it has looked at Docker.
fn cold_start(budget: JobSize) -> (Gate, StartQueue, CreatedContainers) {
  (
    Gate::new(budget, 32),
    StartQueue::new(),
    CreatedContainers::new(),
  )
}

#[test]
fn a_restart_rebuilds_consumed_budget_from_the_running_containers() {
  let (mut gate, mut queue, mut created) = cold_start(TAG_16VCPU_32GB);
  let containers = [
    container(
      "job-a",
      TAG_8VCPU_16GB,
      ContainerLifecycle::Running,
      DEADLINE_MS,
    ),
    container(
      "job-b",
      TAG_2VCPU_4GB,
      ContainerLifecycle::Running,
      DEADLINE_MS,
    ),
  ];

  let adoption = adopt(&mut gate, &mut queue, &mut created, &containers, RESTART_MS);

  let consumption = gate.consumption();
  assert_eq!(consumption.vcpu_used, 10, "8 + 2 vCPU are already in use");
  assert_eq!(consumption.memory_mb_used, 20480, "16GB + 4GB are in use");
  assert_eq!(consumption.queue_depth, 2);
  assert_eq!(adoption.resumed.len(), 2);
  assert!(adoption.remove.is_empty());
  assert!(queue.is_empty(), "a running job is not waiting to start");

  // The deadline the reaper will enforce comes back with each job.
  for armed in &adoption.resumed {
    assert_eq!(armed.deadline_ms, DEADLINE_MS);
  }
  // And the container id is recorded, so a redelivered create is idempotent
  // and a promotion knows what to start.
  assert_eq!(created.existing("job-a"), Some("container-for-job-a"));
}

#[test]
fn the_budget_a_restart_rebuilds_is_the_budget_the_next_job_competes_for() {
  let (mut gate, mut queue, mut created) = cold_start(TAG_16VCPU_32GB);
  let containers = [container(
    "job-a",
    TAG_8VCPU_16GB,
    ContainerLifecycle::Running,
    DEADLINE_MS,
  )];

  adopt(&mut gate, &mut queue, &mut created, &containers, RESTART_MS);

  // 8 vCPU of the box's 16 are gone, so one more 8-vCPU job fits and a
  // second one does not — exactly as if the daemon had never restarted.
  let second = JobId::new("job-b");
  let third = JobId::new("job-c");
  assert!(gate.admit(&second, TAG_8VCPU_16GB).is_ok());
  assert!(gate.admit(&third, TAG_8VCPU_16GB).is_ok());
  assert_eq!(gate.try_start(&second), Some(StartOutcome::Started));
  assert_eq!(
    gate.try_start(&third),
    Some(StartOutcome::Deferred),
    "the adopted job still holds its half of the box"
  );
}

#[test]
fn a_container_past_its_deadline_is_removed_and_charged_nothing() {
  let (mut gate, mut queue, mut created) = cold_start(TAG_16VCPU_32GB);
  let containers = [container(
    "job-late",
    TAG_8VCPU_16GB,
    ContainerLifecycle::Running,
    DEADLINE_MS,
  )];

  let adoption = adopt(
    &mut gate,
    &mut queue,
    &mut created,
    &containers,
    DEADLINE_MS + 1,
  );

  assert_eq!(adoption.remove, vec!["container-for-job-late".to_owned()]);
  assert!(adoption.resumed.is_empty());
  assert_eq!(gate.consumption().vcpu_used, 0);
  assert_eq!(gate.consumption().queue_depth, 0);
}

#[test]
fn a_finished_container_is_removed_rather_than_restarted() {
  let (mut gate, mut queue, mut created) = cold_start(TAG_16VCPU_32GB);
  let containers = [container(
    "job-done",
    TAG_8VCPU_16GB,
    ContainerLifecycle::Finished,
    DEADLINE_MS,
  )];

  let adoption = adopt(&mut gate, &mut queue, &mut created, &containers, RESTART_MS);

  // Restarting it would boot a runner against a JIT config GitHub has
  // already consumed, and charge the gate for work that is over.
  assert_eq!(adoption.remove, vec!["container-for-job-done".to_owned()]);
  assert!(adoption.requeued.is_empty());
  assert!(queue.is_empty());
  assert_eq!(gate.consumption().vcpu_used, 0);
  assert_eq!(created.existing("job-done"), None);
}

#[test]
fn a_created_but_never_started_container_is_requeued_without_charging_budget() {
  let (mut gate, mut queue, mut created) = cold_start(TAG_16VCPU_32GB);
  let containers = [container(
    "job-pending",
    TAG_8VCPU_16GB,
    ContainerLifecycle::Created,
    DEADLINE_MS,
  )];

  let adoption = adopt(&mut gate, &mut queue, &mut created, &containers, RESTART_MS);

  assert_eq!(adoption.requeued.len(), 1);
  assert!(adoption.remove.is_empty());
  assert_eq!(
    queue.len(),
    1,
    "the reaper tick starts it when budget frees"
  );
  assert_eq!(
    created.existing("job-pending"),
    Some("container-for-job-pending"),
    "promotion needs the container id to start"
  );
  assert_eq!(
    gate.consumption().vcpu_used,
    0,
    "an unstarted container holds nothing yet"
  );
  assert_eq!(gate.consumption().queue_depth, 1);
}

#[test]
fn an_adopted_container_that_no_longer_fits_still_holds_its_budget() {
  // The operator narrowed TOOLU_DAEMON_VCPU below what is already running.
  let (mut gate, mut queue, mut created) = cold_start(TAG_2VCPU_4GB);
  let containers = [container(
    "job-big",
    TAG_8VCPU_16GB,
    ContainerLifecycle::Running,
    DEADLINE_MS,
  )];

  let adoption = adopt(&mut gate, &mut queue, &mut created, &containers, RESTART_MS);

  assert_eq!(adoption.resumed.len(), 1, "reality wins over the ceiling");
  assert_eq!(gate.consumption().vcpu_used, 8);
  // Nothing else starts while the box is over budget, which is the point.
  let next = JobId::new("job-next");
  assert!(gate.admit(&next, TAG_2VCPU_4GB).is_ok());
  assert_eq!(gate.try_start(&next), Some(StartOutcome::Deferred));
}

#[test]
fn two_containers_claiming_one_job_id_keep_the_first_and_remove_the_second() {
  let (mut gate, mut queue, mut created) = cold_start(TAG_16VCPU_32GB);
  let containers = [
    AdoptedContainer::new(
      "job-a",
      "container-first",
      DEADLINE_MS,
      TAG_8VCPU_16GB,
      ContainerLifecycle::Running,
    ),
    AdoptedContainer::new(
      "job-a",
      "container-second",
      DEADLINE_MS,
      TAG_8VCPU_16GB,
      ContainerLifecycle::Running,
    ),
  ];

  let adoption = adopt(&mut gate, &mut queue, &mut created, &containers, RESTART_MS);

  assert_eq!(adoption.resumed.len(), 1);
  assert_eq!(adoption.remove, vec!["container-second".to_owned()]);
  assert_eq!(
    gate.consumption().vcpu_used,
    8,
    "the job is charged once, not twice"
  );
}

#[test]
fn an_unstarted_container_the_budget_can_never_fit_is_removed() {
  // A 2-vCPU box that somehow holds an unstarted 8-vCPU container: it could
  // never start, so waiting for it would be waiting forever.
  let (mut gate, mut queue, mut created) = cold_start(TAG_2VCPU_4GB);
  let containers = [container(
    "job-impossible",
    TAG_8VCPU_16GB,
    ContainerLifecycle::Created,
    DEADLINE_MS,
  )];

  let adoption = adopt(&mut gate, &mut queue, &mut created, &containers, RESTART_MS);

  assert_eq!(
    adoption.remove,
    vec!["container-for-job-impossible".to_owned()]
  );
  assert!(queue.is_empty());
  assert_eq!(gate.consumption().queue_depth, 0);
}

#[test]
fn the_deadline_decides_before_the_lifecycle_does() {
  let running = container(
    "job-a",
    TAG_2VCPU_4GB,
    ContainerLifecycle::Running,
    DEADLINE_MS,
  );
  assert_eq!(action_for(&running, RESTART_MS), AdoptionAction::Resume);
  assert_eq!(
    action_for(&running, DEADLINE_MS),
    AdoptionAction::Remove,
    "a deadline exactly reached has passed"
  );

  let pending = container(
    "job-b",
    TAG_2VCPU_4GB,
    ContainerLifecycle::Created,
    DEADLINE_MS,
  );
  assert_eq!(action_for(&pending, RESTART_MS), AdoptionAction::Requeue);
  assert_eq!(
    action_for(&pending, DEADLINE_MS + 1),
    AdoptionAction::Remove
  );
}

#[test]
fn dockers_status_words_map_onto_the_three_lifecycles() {
  assert_eq!(
    lifecycle_of(Some(ContainerStateStatusEnum::CREATED)),
    ContainerLifecycle::Created
  );
  for status in [
    ContainerStateStatusEnum::RUNNING,
    ContainerStateStatusEnum::PAUSED,
    ContainerStateStatusEnum::RESTARTING,
  ] {
    assert_eq!(
      lifecycle_of(Some(status)),
      ContainerLifecycle::Running,
      "{status} still holds its limits on the box"
    );
  }
  for status in [
    ContainerStateStatusEnum::EXITED,
    ContainerStateStatusEnum::DEAD,
    ContainerStateStatusEnum::REMOVING,
    ContainerStateStatusEnum::EMPTY,
  ] {
    assert_eq!(
      lifecycle_of(Some(status)),
      ContainerLifecycle::Finished,
      "{status} has nothing left to charge for"
    );
  }
  assert_eq!(lifecycle_of(None), ContainerLifecycle::Finished);
}

/// The `POST /v1/jobs` body `createVpsInstance` sends for one job of `size`.
fn create_request(job_id: &str, size: JobSize) -> CreateJobRequest {
  CreateJobRequest {
    jit_config: "eyJydW5uZXIiOiJ0b29sdS0xIiwidG9rZW4iOiJzZWNyZXQtaml0In0=".to_owned(),
    image: "ghcr.io/falconiere/toolu-ghrunner:latest".to_owned(),
    size: JobSizeWire {
      vcpu: size.vcpu,
      memory_mb: size.memory_mb,
    },
    job_ref: JobRefWire {
      org: "acme".to_owned(),
      repo: "widgets".to_owned(),
      job_id: job_id.to_owned(),
    },
    purpose: "adoption test".to_owned(),
    deadline: DEADLINE_MS,
  }
}

/// The inspect response Docker returns for a container created from
/// `container_config` — the same labels, environment and limits, echoed
/// back the way `existing_jobs` reads them.
fn inspect_response(
  req: &CreateJobRequest,
  status: ContainerStateStatusEnum,
) -> ContainerInspectResponse {
  let body = container_config(req, "sysbox-runc");
  ContainerInspectResponse {
    config: Some(ContainerConfig {
      labels: body.labels,
      env: body.env,
      image: body.image,
      ..ContainerConfig::default()
    }),
    host_config: body.host_config,
    state: Some(ContainerState {
      status: Some(status),
      ..ContainerState::default()
    }),
    ..ContainerInspectResponse::default()
  }
}

#[test]
fn every_catalog_size_survives_the_round_trip_through_dockers_limits() {
  for size in CATALOG {
    let req = create_request("job-a", size);
    let inspected = inspect_response(&req, ContainerStateStatusEnum::RUNNING);

    let adopted = adopted_from_inspect("container-a", &inspected)
      .expect("a container this daemon created is adoptable");

    assert_eq!(
      adopted.size, size,
      "{size:?} must come back exactly as it went out"
    );
    assert_eq!(adopted.job_id, "job-a");
    assert_eq!(adopted.deadline_ms, DEADLINE_MS);
    assert_eq!(adopted.lifecycle, ContainerLifecycle::Running);
  }
}

#[test]
fn the_deadline_is_read_from_the_label_and_never_from_the_environment() {
  let req = create_request("job-a", TAG_2VCPU_4GB);
  let mut inspected = inspect_response(&req, ContainerStateStatusEnum::RUNNING);

  // Rewrite the environment's copy of the deadline to something else
  // entirely. Adoption must not notice: that block also carries the
  // single-use JIT credential, and nothing outside the container reads it.
  let env = vec![
    format!("{ENV_JIT_CONFIG}=this-must-never-be-read"),
    format!("{ENV_DEADLINE}=1"),
  ];
  if let Some(config) = inspected.config.as_mut() {
    config.env = Some(env);
  }

  let adopted =
    adopted_from_inspect("container-a", &inspected).expect("labels still identify the job");

  assert_eq!(
    adopted.deadline_ms, DEADLINE_MS,
    "the sh.toolu.deadline label is the only deadline adoption reads"
  );
}

#[test]
fn a_container_without_this_daemons_labels_is_not_adopted() {
  let inspected = ContainerInspectResponse {
    config: Some(ContainerConfig {
      labels: Some(HashMap::from([(
        "com.example.app".to_owned(),
        "postgres".to_owned(),
      )])),
      ..ContainerConfig::default()
    }),
    state: Some(ContainerState {
      status: Some(ContainerStateStatusEnum::RUNNING),
      ..ContainerState::default()
    }),
    ..ContainerInspectResponse::default()
  };

  assert_eq!(
    adopted_from_inspect("container-a", &inspected),
    None,
    "someone else's container holds no budget of ours and is not ours to remove"
  );
}
