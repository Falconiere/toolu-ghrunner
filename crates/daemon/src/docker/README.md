# docker/

**What belongs here:** the bollard-facing half of the daemon's Docker
orchestration — the pure container specification (units, labels,
environment), the in-flight job registry and its reap tombstone,
reading this daemon's own containers back off Docker at startup, and
pulling/checking the pinned image. Each file backs exactly one sibling
crate module: `spec` for container creation, `registry` for
`crate::docker`'s create/reap path, `inventory` for `crate::adopt`, and
`image` for `crate::prepull`.

**What does NOT belong here:** the bollard `Docker` client itself,
`create`/`start`/`wait`/`remove` calls, and the tick loop that starts
whatever the resource gate now has room for — those live in the parent
`crate::docker` (`src/docker.rs`), which these four modules support.
HTTP routing lives in `crate::routes`; the resource gate's admit/release
accounting lives in `crate::gate`.

## Contents

| File | Primary item | Purpose |
| --- | --- | --- |
| `spec.rs` | `JobLabels`, `nano_cpus`, `memory_bytes` | The exact `ContainerCreateBody` a job becomes: `NanoCpus`/`Memory` from integer math, the `sh.toolu.job-id` / `sh.toolu.deadline` labels, and the `TOOLU_JITCONFIG` / `TOOLU_DEADLINE` env — never argv. Pure: no bollard client, no clock. |
| `registry.rs` | `JobRegistry`, `TOMBSTONE_TTL` | The in-flight job registry and reap tombstone that let `DELETE /v1/jobs?jobId=…` cancel a create still in flight. Pure state — the caller passes `now`, so every transition is testable without a daemon. |
| `inventory.rs` | `lifecycle_of`, `adopted_from_inspect` | Reads this daemon's own containers back off Docker at startup: list every container carrying `sh.toolu.job-id`, then inspect each to reconstruct its vCPU/memory footprint and re-arm its deadline from the label, not from `Config.Env`. |
| `image.rs` | `DockerBackend::image_present` / `pull_image` / `attempt_pull` | The only place in the crate that pulls. `crate::docker`'s create path never does — a cold pull inside a request would blow the client's 10-second timeout, which is terminal-with-cooldown on its side. |

When you add a file here, add its row above so the index stays current.
There is no `mod.rs`; the parent `docker.rs` is the module root and
declares submodules (`src/foo.rs` declares `mod bar;` for
`src/foo/bar.rs`). Import concrete paths.
