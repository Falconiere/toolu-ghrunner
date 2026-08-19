//! What one job becomes on the Docker side: the exact
//! [`ContainerCreateBody`] `docker create` receives, and the two labels that
//! are the daemon's only durable state.
//!
//! Everything here is pure — no bollard client, no daemon, no clock — so the
//! shape of a job container is decided (and tested) without one. The values
//! it produces are pinned by
//! `docs/toolu/specs/2026-08-19-ovh-compute-and-vps-daemon-design.md`
//! (toolu.sh repo), "Container creation":
//!
//! - `NanoCpus = i64::from(vcpu) * 1_000_000_000` and
//!   `Memory = memoryMb * 1024 * 1024`, both in integer math — the workspace
//!   denies `cast_possible_truncation` and `cast_sign_loss`, and a float path
//!   through a memory limit is how a job silently gets a byte less than it
//!   asked for.
//! - The JIT config travels in the container's environment and **never** in
//!   argv, where a provider dashboard or a `ps` listing would show a
//!   single-use GitHub credential. That is also `docs/container-image.md`'s
//!   contract: the image boots with zero arguments and reads everything from
//!   the environment, so this module sets neither `Cmd` nor `Entrypoint`.
//! - Two labels, `sh.toolu.job-id` and `sh.toolu.deadline`. State lives in
//!   Docker rather than in this process: startup adoption rebuilds every live
//!   job from these, so they must be complete and readable back exactly.

use std::collections::HashMap;
use std::fmt;

use bollard::models::{ContainerCreateBody, HostConfig};

use crate::routes::wire::CreateJobRequest;

/// Label carrying GitHub's own job id — the key
/// `DELETE /v1/jobs?jobId=…` reaps by, and the one startup adoption reads to
/// re-identify a container it did not create.
pub const LABEL_JOB_ID: &str = "sh.toolu.job-id";

/// Label carrying the job's epoch-millisecond deadline, as a decimal string.
/// The reaper reads the deadline from here and never from `Config.Env`: that
/// env block also carries the JIT config, which nothing outside the container
/// may touch.
pub const LABEL_DEADLINE: &str = "sh.toolu.deadline";

/// Environment variable carrying GitHub's `encoded_jit_config` verbatim —
/// `docs/container-image.md`'s required variable.
pub const ENV_JIT_CONFIG: &str = "TOOLU_JITCONFIG";

/// Environment variable carrying the epoch-millisecond deadline the runner's
/// own watchdog arms itself from.
pub const ENV_DEADLINE: &str = "TOOLU_DEADLINE";

/// Nano-CPUs in one whole vCPU: Docker's `NanoCpus` is quoted in 10^-9 CPUs.
const NANO_CPUS_PER_VCPU: i64 = 1_000_000_000;

/// Bytes in one megabyte: Docker's `Memory` limit is quoted in bytes.
const BYTES_PER_MB: i64 = 1024 * 1024;

/// Docker's `NanoCpus` for `vcpu` whole vCPUs.
///
/// `i64::from` widens losslessly and the product cannot overflow: the widest
/// input this type admits, `u32::MAX`, yields 4.29e18, still inside `i64`'s
/// 9.22e18 ceiling. No cast, no float, no saturation — see the module docs.
pub fn nano_cpus(vcpu: u32) -> i64 {
  i64::from(vcpu) * NANO_CPUS_PER_VCPU
}

/// Docker's `Memory` limit, in bytes, for `memory_mb` megabytes.
///
/// `i64` for the same reason [`nano_cpus`] uses it, and it is the reason the
/// arithmetic cannot stay in `u32`: 4096 MB — the smallest tag in the
/// catalog — already overflows a `u32` byte count.
pub fn memory_bytes(memory_mb: u32) -> i64 {
  i64::from(memory_mb) * BYTES_PER_MB
}

/// The whole vCPUs behind a container's `NanoCpus`, the inverse of
/// [`nano_cpus`] — how `crate::adopt` reads a running container's vCPU share
/// back off Docker after a restart, since the labels carry identity and
/// deadline only.
///
/// Integer division, and deliberately truncating: every container this daemon
/// creates carries an exact multiple of [`NANO_CPUS_PER_VCPU`], so a
/// remainder can only come from a container something else created, and
/// rounding one of those up would charge the gate for capacity it does not
/// hold. A negative limit — Docker's "unset" — reads as zero, and a limit too
/// wide for a `u32` saturates rather than wrapping, which fails safe in the
/// one direction that matters: an over-reported footprint starts nothing new,
/// an under-reported one overcommits the box.
pub fn vcpu_from_nano_cpus(nano_cpus: i64) -> u32 {
  saturating_u32(nano_cpus / NANO_CPUS_PER_VCPU)
}

/// The megabytes behind a container's `Memory` limit, the inverse of
/// [`memory_bytes`]. Truncating and saturating for the same reasons as
/// [`vcpu_from_nano_cpus`].
pub fn memory_mb_from_bytes(bytes: i64) -> u32 {
  saturating_u32(bytes / BYTES_PER_MB)
}

/// Narrow to `u32` without an `as` cast: negatives floor at zero, anything
/// too wide saturates at the ceiling. Both ends are unreachable for a
/// container this daemon created and are the conservative choice for one it
/// did not.
fn saturating_u32(value: i64) -> u32 {
  u32::try_from(value).unwrap_or(if value.is_negative() { 0 } else { u32::MAX })
}

/// The two labels a job container carries, parsed back into the values that
/// produced them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobLabels {
  /// GitHub's own job id, as text — the `sh.toolu.job-id` label.
  pub job_id: String,
  /// Epoch milliseconds the job must finish by — the `sh.toolu.deadline`
  /// label.
  pub deadline: i64,
}

/// Why a container's label map is not a readable [`JobLabels`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LabelError {
  /// The named label is absent — the container was not created by this
  /// daemon, so it holds no job of ours.
  Missing(&'static str),
  /// `sh.toolu.deadline` is present but is not a decimal epoch-ms integer.
  /// Distinct from [`Self::Missing`] on purpose: a container of ours whose
  /// deadline cannot be read is a container the reaper must still deal with,
  /// where a container without our labels is simply none of our business.
  InvalidDeadline(String),
}

impl fmt::Display for LabelError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Missing(label) => write!(f, "container carries no {label} label"),
      Self::InvalidDeadline(value) => {
        write!(f, "{LABEL_DEADLINE} is not epoch milliseconds: {value:?}")
      },
    }
  }
}

impl std::error::Error for LabelError {}

impl JobLabels {
  /// The labels for `job_id` finishing by `deadline` (epoch milliseconds).
  pub fn new(job_id: impl Into<String>, deadline: i64) -> Self {
    Self {
      job_id: job_id.into(),
      deadline,
    }
  }

  /// Render as the `Labels` map `docker create` takes.
  pub fn to_map(&self) -> HashMap<String, String> {
    HashMap::from([
      (LABEL_JOB_ID.to_owned(), self.job_id.clone()),
      (LABEL_DEADLINE.to_owned(), self.deadline.to_string()),
    ])
  }

  /// Read the labels back off an inspected container — the round trip
  /// startup adoption depends on.
  ///
  /// # Errors
  ///
  /// Returns [`LabelError::Missing`] when either label is absent, and
  /// [`LabelError::InvalidDeadline`] when the deadline label is present but
  /// unparseable.
  pub fn from_map(labels: &HashMap<String, String>) -> Result<Self, LabelError> {
    let job_id = labels
      .get(LABEL_JOB_ID)
      .ok_or(LabelError::Missing(LABEL_JOB_ID))?;
    let raw_deadline = labels
      .get(LABEL_DEADLINE)
      .ok_or(LabelError::Missing(LABEL_DEADLINE))?;
    let deadline = raw_deadline
      .parse::<i64>()
      .map_err(|_parse_err| LabelError::InvalidDeadline(raw_deadline.clone()))?;

    Ok(Self {
      job_id: job_id.clone(),
      deadline,
    })
  }
}

/// The `docker create` body for `req`, to be created under `runtime`
/// (`sysbox-runc` unless the host overrode `TOOLU_DAEMON_RUNTIME`).
///
/// `Cmd` and `Entrypoint` are deliberately left unset: the image boots with
/// zero arguments, and anything placed there would put the job's contract —
/// including a single-use JIT credential — into a process listing.
pub fn container_config(req: &CreateJobRequest, runtime: &str) -> ContainerCreateBody {
  let labels = JobLabels::new(req.job_ref.job_id.clone(), req.deadline);

  ContainerCreateBody {
    image: Some(req.image.clone()),
    env: Some(vec![
      format!("{ENV_JIT_CONFIG}={}", req.jit_config),
      format!("{ENV_DEADLINE}={}", req.deadline),
    ]),
    labels: Some(labels.to_map()),
    host_config: Some(HostConfig {
      runtime: Some(runtime.to_owned()),
      nano_cpus: Some(nano_cpus(req.size.vcpu)),
      memory: Some(memory_bytes(req.size.memory_mb)),
      ..HostConfig::default()
    }),
    ..ContainerCreateBody::default()
  }
}
