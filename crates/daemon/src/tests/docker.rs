//! Tests for the daemon's Docker orchestration — every part of it that holds
//! without a Docker daemon.
//!
//! Real data throughout: the sizes are the rows of
//! `packages/api/src/github/runner-tags.ts` (toolu.sh repo), the request
//! bodies are the JSON `providers/vps/client.ts` posts, and the failures are
//! real `bollard` error values. Nothing here is a shadow type standing in for
//! a real one: the container specification is asserted on the actual
//! `ContainerCreateBody` that reaches `docker create`.
//!
//! Nothing here connects to Docker, deliberately. CI runs
//! `cargo test --workspace` on `ubuntu-latest` **and** `macos-14`, and the
//! macOS leg has no Docker daemon at all — a test that dialled one would fail
//! CI outright. Real-Docker behaviour is the live suite's job.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use bollard::errors::Error as DockerError;

use super::registry::{BeginOutcome, FinishOutcome, JobRegistry, TOMBSTONE_TTL};
use super::spec::{
  ENV_DEADLINE, ENV_JIT_CONFIG, JobLabels, LABEL_DEADLINE, LABEL_JOB_ID, LabelError,
  container_config, memory_bytes, nano_cpus,
};
use super::{HTTP_NOT_FOUND, classify_create_error};
use crate::routes::backend::CreateError;
use crate::routes::wire::CreateJobRequest;

/// A stand-in for GitHub's `encoded_jit_config`: base64 text, single-use, and
/// the one value in a request that must never reach a process listing.
const JIT_CONFIG: &str = "eyJydW5uZXIiOiJ0b29sdS0xIiwidG9rZW4iOiJzZWNyZXQtaml0In0=";

/// Epoch milliseconds — what `client.ts` sends as `deadline` (`nowMs` plus
/// six hours).
const DEADLINE_MS: i64 = 1_763_000_000_000;

/// The runtime `TOOLU_DAEMON_RUNTIME` defaults to.
const RUNTIME: &str = "sysbox-runc";

/// Every Linux tag `runner-tags.ts` sells, plus the widest macOS one, with
/// the Docker units each must produce. Written out rather than computed: a
/// test that restated the formula would agree with any formula, including a
/// wrong one.
const CATALOG: [(u32, u32, i64, i64); 6] = [
  // toolu-ubuntu / toolu-ubuntu-2vcpu-4gb — the default, and the floor.
  (2, 4096, 2_000_000_000, 4_294_967_296),
  // toolu-ubuntu-4vcpu-8gb
  (4, 8192, 4_000_000_000, 8_589_934_592),
  // toolu-ubuntu-8vcpu-16gb
  (8, 16384, 8_000_000_000, 17_179_869_184),
  // toolu-ubuntu-8vcpu-32gb
  (8, 32768, 8_000_000_000, 34_359_738_368),
  // toolu-ubuntu-16vcpu-32gb — the largest tag this daemon can be asked for.
  (16, 32768, 16_000_000_000, 34_359_738_368),
  // toolu-macos-12vcpu-56gb — the largest shape in the catalog at all.
  (12, 57344, 12_000_000_000, 60_129_542_144),
];

/// The exact `POST /v1/jobs` body `createVpsInstance` sends, for one job.
fn request_json(job_id: &str, vcpu: u32, memory_mb: u32) -> String {
  format!(
    r#"{{
      "jitConfig": "{JIT_CONFIG}",
      "image": "ghcr.io/falconiere/toolu-ghrunner:latest-docker",
      "size": {{ "vcpu": {vcpu}, "memoryMb": {memory_mb} }},
      "jobRef": {{ "org": "acme", "repo": "widgets", "jobId": "{job_id}" }},
      "purpose": "build acme/widgets",
      "deadline": {DEADLINE_MS}
    }}"#
  )
}

/// Parse that body the way the route does — through `serde`, not a hand-built
/// struct, so these tests break if the wire shape drifts.
fn request(job_id: &str, vcpu: u32, memory_mb: u32) -> CreateJobRequest {
  serde_json::from_str(&request_json(job_id, vcpu, memory_mb)).expect("parse create request body")
}

/// The `HostConfig` a request turns into, which is where all three resource
/// decisions land.
fn host_config_for(vcpu: u32, memory_mb: u32) -> bollard::models::HostConfig {
  container_config(&request("77", vcpu, memory_mb), RUNTIME)
    .host_config
    .expect("host config")
}

#[test]
fn every_catalog_size_converts_to_dockers_own_units() {
  for (vcpu, memory_mb, expected_nano_cpus, expected_memory) in CATALOG {
    assert_eq!(
      nano_cpus(vcpu),
      expected_nano_cpus,
      "NanoCpus for {vcpu} vCPU"
    );
    assert_eq!(
      memory_bytes(memory_mb),
      expected_memory,
      "Memory for {memory_mb} MB"
    );

    let host_config = host_config_for(vcpu, memory_mb);
    assert_eq!(host_config.nano_cpus, Some(expected_nano_cpus));
    assert_eq!(host_config.memory, Some(expected_memory));
  }
}

/// The conversions have to be `i64`, and they have to be exact at the widest
/// value the wire type admits. `u32` bytes would have wrapped at the
/// *smallest* tag in the catalog, silently handing a 4GB job a limit of zero.
#[test]
fn the_conversions_are_exact_at_the_widest_value_a_request_can_carry() {
  assert!(
    memory_bytes(4096) > i64::from(u32::MAX),
    "4096 MB in bytes already exceeds u32::MAX — the byte count cannot live in u32"
  );

  assert_eq!(nano_cpus(u32::MAX), 4_294_967_295_000_000_000);
  assert_eq!(memory_bytes(u32::MAX), 4_503_599_626_321_920);
  assert!(nano_cpus(u32::MAX) < i64::MAX);
  assert!(memory_bytes(u32::MAX) < i64::MAX);

  assert_eq!(nano_cpus(0), 0);
  assert_eq!(memory_bytes(0), 0);
}

#[test]
fn the_container_takes_the_requested_image_and_the_hosts_runtime() {
  let config = container_config(&request("77", 16, 32768), RUNTIME);

  assert_eq!(
    config.image.as_deref(),
    Some("ghcr.io/falconiere/toolu-ghrunner:latest-docker")
  );
  assert_eq!(
    config.host_config.and_then(|host| host.runtime).as_deref(),
    Some(RUNTIME)
  );
}

/// The JIT config is a single-use GitHub credential. In the environment it is
/// visible only inside the container; in argv it would show up in the host's
/// process listing and in `docker ps`.
#[test]
fn the_jit_config_travels_in_the_environment_and_never_in_argv() {
  let config = container_config(&request("77", 2, 4096), RUNTIME);
  let env = config.env.expect("env block");

  assert!(env.contains(&format!("{ENV_JIT_CONFIG}={JIT_CONFIG}")));
  assert!(env.contains(&format!("{ENV_DEADLINE}={DEADLINE_MS}")));

  // The image boots with zero arguments (docs/container-image.md), so both
  // argv sources stay unset — and neither may carry the secret by any route.
  assert_eq!(config.cmd, None);
  assert_eq!(config.entrypoint, None);

  let labels = config.labels.expect("labels");
  assert!(
    !labels.values().any(|value| value.contains(JIT_CONFIG)),
    "the JIT config must not reach `docker inspect` output either"
  );
}

#[test]
fn the_two_labels_are_written_and_read_back_unchanged() {
  let config = container_config(&request("4242", 8, 16384), RUNTIME);
  let labels = config.labels.expect("labels");

  assert_eq!(labels.get(LABEL_JOB_ID).map(String::as_str), Some("4242"));
  assert_eq!(
    labels.get(LABEL_DEADLINE).map(String::as_str),
    Some("1763000000000")
  );

  let parsed = JobLabels::from_map(&labels).expect("labels parse back");
  assert_eq!(parsed, JobLabels::new("4242", DEADLINE_MS));
  assert_eq!(parsed.to_map(), labels);
}

#[test]
fn a_container_without_our_labels_is_not_ours_to_adopt() {
  let foreign = HashMap::from([("com.example.app".to_owned(), "postgres".to_owned())]);
  assert_eq!(
    JobLabels::from_map(&foreign),
    Err(LabelError::Missing(LABEL_JOB_ID))
  );

  let half = HashMap::from([(LABEL_JOB_ID.to_owned(), "77".to_owned())]);
  assert_eq!(
    JobLabels::from_map(&half),
    Err(LabelError::Missing(LABEL_DEADLINE))
  );
}

/// A container of ours whose deadline cannot be read is a different problem
/// from a container that is none of our business, and the reaper has to be
/// able to tell them apart.
#[test]
fn an_unreadable_deadline_label_is_reported_as_such() {
  let broken = HashMap::from([
    (LABEL_JOB_ID.to_owned(), "77".to_owned()),
    (LABEL_DEADLINE.to_owned(), "soon".to_owned()),
  ]);
  assert_eq!(
    JobLabels::from_map(&broken),
    Err(LabelError::InvalidDeadline("soon".to_owned()))
  );
}

#[test]
fn an_untouched_create_keeps_its_container() {
  let mut registry = JobRegistry::new();
  let now = Instant::now();

  assert_eq!(registry.begin_create("77", now), BeginOutcome::Proceed);
  assert_eq!(
    registry.finish_create("77", "container-a", now),
    FinishOutcome::Keep
  );
  assert_eq!(
    registry.job_for_container("container-a"),
    Some("77".to_owned())
  );
}

/// The ordering this whole registry exists for: the reap lands while
/// `docker create` is still in flight, so there is no container to kill yet —
/// and the container that arrives a moment later must be discarded, never
/// started.
#[test]
fn a_reap_during_an_in_flight_create_discards_the_container_that_lands() {
  let mut registry = JobRegistry::new();
  let now = Instant::now();

  assert_eq!(registry.begin_create("77", now), BeginOutcome::Proceed);
  assert_eq!(
    registry.reap("77", now),
    None,
    "no container exists yet, so there is nothing to remove — only to mark"
  );
  assert_eq!(
    registry.finish_create("77", "container-a", now),
    FinishOutcome::DiscardReaped
  );
  assert_eq!(
    registry.job_for_container("container-a"),
    None,
    "a discarded container is not a live job"
  );
}

/// The reap can also overtake the create entirely — the client's request
/// timed out before the handler reached Docker at all.
#[test]
fn a_reap_that_arrives_first_stops_the_create_from_running() {
  let mut registry = JobRegistry::new();
  let now = Instant::now();

  assert_eq!(registry.reap("77", now), None);
  assert_eq!(
    registry.begin_create("77", now),
    BeginOutcome::AlreadyReaped
  );
}

#[test]
fn a_reap_after_the_container_exists_names_it_for_removal() {
  let mut registry = JobRegistry::new();
  let now = Instant::now();

  registry.begin_create("77", now);
  registry.finish_create("77", "container-a", now);

  assert_eq!(registry.reap("77", now), Some("container-a".to_owned()));
  assert_eq!(
    registry.reap("77", now),
    None,
    "the second reap has nothing left to remove"
  );
}

#[test]
fn a_second_create_for_a_job_already_in_flight_is_refused() {
  let mut registry = JobRegistry::new();
  let now = Instant::now();

  assert_eq!(registry.begin_create("77", now), BeginOutcome::Proceed);
  assert_eq!(
    registry.begin_create("77", now),
    BeginOutcome::AlreadyTracked
  );
}

#[test]
fn a_failed_create_frees_the_job_id_for_a_retry() {
  let mut registry = JobRegistry::new();
  let now = Instant::now();

  registry.begin_create("77", now);
  registry.abandon_create("77");
  assert_eq!(registry.begin_create("77", now), BeginOutcome::Proceed);
}

/// A reaped job stays reaped: giving up a create must not resurrect a job id
/// the client has already been told is gone.
#[test]
fn abandoning_a_create_leaves_a_tombstone_standing() {
  let mut registry = JobRegistry::new();
  let now = Instant::now();

  registry.begin_create("77", now);
  registry.reap("77", now);
  registry.abandon_create("77");

  assert_eq!(
    registry.begin_create("77", now),
    BeginOutcome::AlreadyReaped
  );
}

/// Tombstones are bounded. `vps/verify.ts` probes a host's credentials by
/// reaping a sentinel job id that matches nothing, so a tombstone that lived
/// forever would be one leaked entry per credential check.
#[test]
fn a_tombstone_expires_and_stops_blocking_the_job_id() {
  let mut registry = JobRegistry::new();
  let reaped_at = Instant::now();
  let much_later = reaped_at + TOMBSTONE_TTL + Duration::from_secs(1);

  registry.reap("sentinel", reaped_at);
  assert_eq!(
    registry.begin_create("sentinel", reaped_at + Duration::from_secs(1)),
    BeginOutcome::AlreadyReaped,
    "still inside the retention window"
  );

  let mut later_registry = JobRegistry::new();
  later_registry.reap("sentinel", reaped_at);
  assert_eq!(
    later_registry.begin_create("sentinel", much_later),
    BeginOutcome::Proceed
  );
}

#[test]
fn a_forgotten_job_leaves_nothing_behind() {
  let mut registry = JobRegistry::new();
  let now = Instant::now();

  registry.begin_create("77", now);
  registry.finish_create("77", "container-a", now);
  registry.forget("77");

  assert_eq!(registry.job_for_container("container-a"), None);
  assert_eq!(registry.begin_create("77", now), BeginOutcome::Proceed);
}

/// Docker answers `POST /containers/create` with a 404 for one reason only:
/// the image is not resident. That is the case the daemon must answer with a
/// fast 503 instead of pulling inside the request.
#[test]
fn a_404_from_docker_create_means_the_image_is_not_resident() {
  let err = DockerError::DockerResponseServerError {
    status_code: HTTP_NOT_FOUND,
    message: "No such image: ghcr.io/falconiere/toolu-ghrunner:latest-docker".to_owned(),
  };

  assert!(matches!(
    classify_create_error(&err),
    CreateError::ImageNotResident
  ));
}

#[test]
fn any_other_docker_failure_carries_its_own_reason() {
  let err = DockerError::DockerResponseServerError {
    status_code: 500,
    message: "unknown runtime specified sysbox-runc".to_owned(),
  };

  let reason = match classify_create_error(&err) {
    CreateError::Other(reason) => reason,
    CreateError::ImageNotResident => String::new(),
  };
  assert!(
    reason.contains("unknown runtime specified sysbox-runc"),
    "the daemon's log and the client's 503 both need Docker's own words; got {reason:?}"
  );
}
