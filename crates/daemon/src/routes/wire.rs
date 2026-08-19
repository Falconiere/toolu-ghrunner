//! Wire types for the daemon's HTTP surface — the exact JSON shapes
//! `packages/api/src/providers/vps/client.ts` (toolu.sh repo) sends and
//! reads. Field names are pinned to that client's camelCase JSON via
//! `serde(rename_all = "camelCase")`; Rust field names stay snake_case.
//!
//! `Serialize` on the request types and `Deserialize` on the response type
//! are not exercised by the router itself (the daemon only ever deserializes
//! a request and serializes a response) — they exist so `tests/fixture.rs`
//! can round-trip the shared contract fixture through these exact types
//! rather than a hand-written duplicate.

use serde::{Deserialize, Serialize};

/// `POST /v1/jobs` request body.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateJobRequest {
  /// Base64 GitHub JIT runner config; the backend injects it as the
  /// `TOOLU_JITCONFIG` env var, never argv.
  pub jit_config: String,
  /// OCI image reference. Must already be resident on the host — see
  /// `crates/daemon/README.md`'s "never pull inside a request" invariant.
  pub image: String,
  /// The guest shape this job's runner tag asked for.
  pub size: JobSizeWire,
  /// Which GitHub job this container will run, for container labels and
  /// logs.
  pub job_ref: JobRefWire,
  /// Free text for the daemon's own logs/audit surface.
  pub purpose: String,
  /// Epoch ms the job must finish by.
  pub deadline: i64,
}

/// `size` on [`CreateJobRequest`] — vCPU and memory, the two units every
/// caller agrees on.
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobSizeWire {
  /// Whole vCPUs.
  pub vcpu: u32,
  /// Memory, in megabytes.
  pub memory_mb: u32,
}

/// `jobRef` on [`CreateJobRequest`] — identifies the GitHub job this
/// container serves.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct JobRefWire {
  /// GitHub organization login.
  pub org: String,
  /// GitHub repository name.
  pub repo: String,
  /// GitHub's own job id, as text.
  #[serde(rename = "jobId")]
  pub job_id: String,
}

/// `POST /v1/jobs` success body. `client.ts` reads only `containerId`; this
/// daemon does not emit any other field.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateJobResponse {
  /// The created container's id — the handle `destroyVpsInstance` later
  /// addresses via `DELETE /v1/jobs/{containerId}`.
  pub container_id: String,
}

/// `DELETE /v1/jobs?jobId=…` query parameters.
#[derive(Debug, Clone, Deserialize)]
pub struct ReapQuery {
  /// GitHub's own job id, as text — matched against whatever container is
  /// currently serving it, if any.
  #[serde(rename = "jobId")]
  pub job_id: String,
}
