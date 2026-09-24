# docker/

**What belongs here:** the bollard wrapper for talking to the Docker daemon —
connecting, image pull/inspect, container lifecycle, and service-container
startup, plus host/container path translation.

**What does NOT belong here:** dispatching a job step to a container action —
that is `execution::handlers::docker` (the `runs.using: docker` handler),
which calls into this module. Job-level cgroup wiring lives in
`execution::cgroup_join`, not here.

## Contents

| File | Primary item | Purpose |
| --- | --- | --- |
| `client.rs` | `DockerClient`, `resolve_docker_host` | Thin async wrapper over bollard: resolve the daemon endpoint from `DOCKER_HOST` (default `unix:///var/run/docker.sock`) and connect, pull/inspect images, create/start/wait/remove/kill containers. |
| `path_translator.rs` | `PathTranslator` | Maps host paths (workspace, temp) to their `/github/workspace` and `/github/runner_temp` container equivalents and back. |
| `container_command.rs` | `ContainerCommand` | Connects the local Docker socket and masks daemon diagnostics. |
| `container_spec.rs` | `ContainerSpec` | Evaluates and validates job-container declarations. |
| `container_options.rs` | `parse_container_options` | Parses the supported quoted options without a shell. |
| `container_create_options.rs` | `apply_container_options` | Maps resource, identity and security options to Docker API fields. |
| `container_ports.rs` | `apply_ports` | Validates and maps published port declarations. |
| `container_mounts.rs` | `ContainerMounts` | Mounts individual workspace/runtime directories and a private job home. |
| `container_exec.rs` | `ContainerExec` | Runs attached container processes with timeout/cancel and streamed output. |
| `job_container.rs` | `JobContainer` | Owns the per-job container/network through post steps and explicit cleanup. |
| `services.rs` | `start_service` | Starts a `services:` container on the job network, using `DockerClient` and `path_translator`'s naming conventions. |

When you add a file here, add its row above so the index stays current. There
is no `mod.rs`; the parent `docker.rs` is the module root and declares
submodules (`src/foo.rs` declares `mod bar;` for `src/foo/bar.rs`). Import
concrete paths.
