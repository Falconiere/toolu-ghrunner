# docker/

**What belongs here:** the bollard wrapper for talking to the Docker daemon —
connecting, image pull/inspect, container lifecycle, and service-container
startup, plus host/container path translation.

**What does NOT belong here:** dispatching a job step to a container action —
that is `execution::docker_action` and `execution::docker_stage`, which call
into this module. Job-level cgroup wiring lives in
`execution::cgroup_join`, not here.

## Contents

| File | Primary item | Purpose |
| --- | --- | --- |
| `action_archive.rs` | `archive_context` | Builds Docker contexts using Docker-compatible `.dockerignore` matching and Dockerfile-specific precedence. |
| `action_container.rs` | `ActionContainer` | Owns one attached Docker container per action stage, including timeout/cancellation cleanup. |
| `action_image.rs` | `ActionContainer::prepare_image` | Pulls registry images and builds action Dockerfiles with inspected immutable-cache tags. |
| `action_mounts.rs` | `ActionMounts`, `action_path_to_host` | Mounts standard `/github` paths and translates action-emitted paths back to host coordinates. |
| `client.rs` | `DockerClient`, `resolve_docker_host` | Thin async wrapper over bollard: resolve the daemon endpoint from `DOCKER_HOST` (default `unix:///var/run/docker.sock`) and connect, pull/inspect images, create/start/wait/remove/kill containers. |
| `path_translator.rs` | `PathTranslator` | Maps host paths (workspace, temp) to their `/github/workspace` and `/github/runner_temp` container equivalents and back. |
| `container_command.rs` | `ContainerCommand` | Connects the local Docker socket and masks daemon diagnostics. |
| `container_spec.rs` | `ContainerSpec` | Evaluates and validates job-container declarations. |
| `container_options.rs` | `parse_container_options` | Parses the supported quoted options without a shell. |
| `container_create_options.rs` | `apply_container_options` | Maps resource, identity and security options to Docker API fields. |
| `container_health_options.rs` | `apply_health_options` | Maps health-check CLI options to Docker's typed `Healthcheck` create field. |
| `container_ports.rs` | `apply_ports` | Validates and maps published port declarations. |
| `container_mounts.rs` | `ContainerMounts` | Mounts individual workspace/runtime directories and a private job home. |
| `container_exec.rs` | `ContainerExec` | Runs attached container processes with timeout/cancel and streamed output. |
| `job_container.rs` | `JobContainer` | Owns the per-job container/network through post steps and explicit cleanup. |
| `services.rs` | `ServiceContainers` | Owns service startup, context metadata, diagnostic logs and cleanup on shared or host-owned networks. |
| `service_spec.rs` | `ServiceSpec` | Evaluates ordered acquired service declarations and validates platform support. |
| `service_create.rs` | Service create helpers | Constructs Docker requests and pulls with registry credentials. |
| `service_health.rs` | Health readiness | Waits with a cancellable, bounded health-check budget. |
| `tests/service_containers_resources.rs` | Integration resource cases | Included by the service-container integration target for real Docker options, sentinel ownership and registry-auth checks. |

When you add a file here, add its row above so the index stays current. There
is no `mod.rs`; the parent `docker.rs` is the module root and declares
submodules (`src/foo.rs` declares `mod bar;` for `src/foo/bar.rs`). Import
concrete paths.
