# Service containers — Design

**Date:** 2026-09-25   **Status:** Approved   **Author:** Codex   **Topic:** #74 services on host and container jobs

## Problem

Acquired `jobServiceContainers` is discarded and `job.services` is empty. Jobs silently run without declared databases or caches. #73 now provides job-container networking and masked Docker transport.

## Non-Goals

1. Implement Docker actions (#75), container hooks, or Windows support.
2. Refactor job-container network ownership or change local artifact/cache service modes.
3. Claim unexecuted GitHub.com/GHES/reference lanes as passing.

## Architecture

Deserialize the optional service template token in shared. Evaluate the mapping in wire order before workspace setup; reuse ContainerSpec evaluation, volume validation, port publishing, and Docker create-option translation. Add service health options to the typed option translator. Invalid declarations and non-Linux hosts fail before user steps.

A ServiceContainers group owns all service containers. On container jobs it borrows JobContainer.network; on host jobs it creates and owns one UUID-named network. Create all services with their IDs as DNS aliases, preserving image entrypoints/commands. Register all credentials with the job masker before connecting or pulling. Pull with DockerCredentials through the existing masked transport, retaining registry ports/tags/digests. Retain names before create requests so partial setup can be cleaned. Await mutations before cancellation checks to avoid late-created orphan resources.

Start services after the job container and before prepared::execute. Wait for readiness before any main/pre/post user action. Copy actual inspected IDs/network/ports into ExecutionContext. All main and post steps retain services. On every result, dump service logs through RunnerEvent::Log with an empty step ID (job-level log), remove service containers, remove a host-owned network, then finish the job container. Cleanup attempts every owned resource even after failures, preserves the primary error, and makes cleanup failures visible. Never prune unrelated resources or remove user-supplied volumes.

## Interfaces / Schema

- `AgentJobRequestMessage.job_service_containers: Option<TemplateToken>` maps `jobServiceContainers`.
- `ServiceSpec { alias: String, container: ContainerSpec }`; `evaluate_services(token, ctx)` yields an ordered Vec. Absent/null/empty mapping yields none; empty image disables a service as upstream does.
- `ServiceContainers::start(specs, borrowed_network, masker, cancel, events)` returns a ready group or cleans partial resources and fails; cancellation is reported as Cancelled by the engine. `cleanup(events)` dumps logs and removes resources independently of the cancelled token.
- `job.services.<alias>` is `{id: string, network: string, ports: {container_port: host_port_string}}` from Docker inspection. Use first binding for each container port, stripping `/protocol`, matching upstream context convention.
- Health options: `--health-cmd`, `--health-interval`, `--health-timeout`, `--health-retries`, `--health-start-period`, `--health-start-interval`, `--no-healthcheck`. Parse duration and integer values without shell execution; reject malformed/overflow values and topology overrides.
- Health: absent/disabled check passes immediately; healthy passes; unhealthy fails; starting waits with exponential 2s to 32s backoff, cancellable. Overall per-service startup budget is 300s. This explicit bound fulfills the issue; pinned upstream instead waits until health resolves or job cancellation. This difference must be documented, not called exact timing parity.

## Failure modes and edge cases

Bad mapping/scalar/alias/config fails visibly without steps. Empty/null declaration creates no Docker resources, including on macOS. Nonempty services reject non-Linux hosts. Duplicate aliases fail before mutation. Credentials and their escaped forms are masked in diagnostics/events. Image pull/auth error, fixed-port conflict, early exit with health enabled, unhealthy or never healthy service prevents steps and triggers log dump/cleanup. No-healthcheck startup adds no application readiness guarantee. A second-service failure cleans the first. Cancellation during pull/health wait and during execution cleans all owned resources; health sleep listens to cancellation. Cleanup runs after post steps and without cancellation. A closed event receiver must not prevent cleanup. Concurrent jobs use unique names/networks. Local user volumes survive.

## Acceptance criteria

- **AC-1:** Acquired service declarations run on Linux; malformed or unsupported declarations fail before any user step. Absent/empty services preserve host execution (74-S5).
- **AC-2:** A real host client connects through the dynamically published `job.services` port; a container client connects by service DNS alias. IDs and network are actual Docker resources (74-S1).
- **AC-3:** Healthy/delayed/no-healthcheck services permit steps under the defined readiness contract; unhealthy/never-ready services fail within the budget with diagnostic logs (74-S2).
- **AC-4:** Multiple services receive evaluated env/options/volumes and authenticated image pulls; fixed-port conflicts fail visibly and credentials remain masked (74-S3).
- **AC-5:** Success, second-service failure and cancellation during health wait/job remove owned containers/network, preserve unrelated resources and retain logs (74-S4).
- **AC-6:** Full repository gate passes and docs/evidence map state tested platforms/backends, exact commands and unverified lanes.

## Acceptance evidence

| AC | Real input and observable | Runnable check |
| --- | --- | --- |
| AC-1 | Acquired #73 payload retains wire fields/types with documented service-token variation; engine rejects macOS/malformed values before a marker step, absence still runs | `./tools/check.sh all`; sibling `service_containers_test` production replay |
| AC-2 | Real Docker HTTP service response and shell client; dynamic host port read from expression, container DNS alias; inspect IDs then assert 404 after teardown | Linux `cargo test -p execution --test service_containers_test -- --ignored --nocapture`; same workflow on pinned official runner |
| AC-3 | Real image health commands: true, delayed file, false, long starting period, absent; assert user-step markers only on successful startup and logs on failure; short internal budget test exercises real never-healthy daemon | Same production replay plus sibling Docker service tests using real Docker |
| AC-4 | Two real services with expression env, volume payload, resource/health options; disposable authenticated local registry and wrong credentials; actual fixed port listener conflict | Same Linux real-Docker lane; credential masking assertions use engine's shared masker; no mocked registry |
| AC-5 | Observed first service start followed by real second-image failure, observed health wait cancellation, observed running-step cancellation; retain sentinel unrelated container/network | Same Linux real-Docker lane; inspect owned resource absence and sentinel survival |
| AC-6 | Gate exit 0; committed scenario/workflow/fixture provenance matrix with commands, platform/backend and real passing links | `./tools/check.sh all`, README and docs/test-coverage.md review |

Full parity evidence requires captured service payload and paired GitHub.com/reference runs using the same workflow/action revisions. Existing captured job payload with explicit variations supports deterministic production replay, not a claim of an acquired service capture. GHES requires a real acquired job; absent infrastructure remains unverified. Linux ARM64 Docker is locally available. macOS supports the explicit rejection lane, not services. #75 integration is recorded as a dependency. No ignored test or zero-test Darwin invocation counts as evidence.

## Documentation impact

README service support/options/readiness/platform rules; docs/architecture.md lifecycle ownership; docs/test-coverage.md all AC and 74-S1–S5 commands/results/provenance. Module READMEs list new files. Durable spec lives under docs/specs consistent with committed epic specs; execution ledger stays under ignored docs/toolu/plans.

## Open Questions

None for implementation. The issue requires bounded startup; use 300s with explicit upstream difference. Capture/reference/GHES availability limits the parity evidence, never inferred from local tests. The orchestrator explicitly authorized delivery on 2026-09-25 with those unavailable lanes documented as unverified; no GHES endpoint/token exists for this epic run. This narrows the delivery gate, not the parity claim. The brief authorizes decisions without a human design round.
