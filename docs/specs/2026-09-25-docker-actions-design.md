# Docker container actions — Design

**Date:** 2026-09-25   **Status:** Approved   **Author:** Codex   **Topic:** #75 / epic #67

## Problem

Docker manifests are parsed incompletely and fail at dispatch. Registry references
never reach execution. Workflows cannot run either kind of container action.

## Non-Goals

1. Implement service orchestration (#74), container hooks, Windows, or OCI actions.
2. Change Node action semantics or report unexecuted parity lanes as passing.
3. Merge this branch; the epic orchestrator owns the merge queue.

## Architecture

Dispatch registry references before repository resolution. Preserve the complete
image reference, including registry ports and digests. Repository and local
manifests use the normal action fetcher and manifest loader. A Docker-specific
stage adapter reuses input/env construction, stdout command dispatch, file-command
application, StepBounds, and the LIFO post drain added by #101.

Use the existing bollard transport and path translator. Each stage creates an
owned container, attaches output before start, waits under cancellation/deadline,
and force-removes the container before returning on all exits. Setup mutations
are awaited before cancellation cleanup so removal cannot race an unfinished create.
Image build/pull errors propagate as step failures. Build contexts are tar archives
of the action Dockerfile directory. Only immutable remote SHA actions reuse a
successfully inspected build image; local actions build against current contents.
Docker's own layer cache remains enabled. Build cache tags include action identity,
SHA, Dockerfile path and platform; cache hits must resolve to an existing image.

The production dispatch rejects non-Linux hosts before Docker setup. Tests of the
actual production path run in a Linux carrier on the available real Linux daemon;
macOS exercises explicit rejection. Registry preparation does not enter the remote
GitHub action prefetch. Existing job.container.network is the attachment contract
for job and future service-only networks; #74 owns creating such networks.

## Interfaces / Schema

- ActionRuns preserves optional args (including absent versus empty), env,
  entrypoint, pre-entrypoint, post-entrypoint, pre-if and post-if.
- DockerStage holds step, manifest, action directory, image, context, events,
  workspace, config, stage name, logging id and StepBounds.
- PostStep carries the prepared Docker image identity in addition to its existing
  manifest/state/scope. Post drain dispatches by RunsUsing; state stays keyed by
  runtime step UUID and outputs by contextName.
- Manifest args are individually interpolated with action inputs and passed as
  argv, preserving empty values and spaces. Manifest entrypoint wins over the
  legacy with.entrypoint fallback. Absent args uses parsed with.args; an explicit
  empty args list overrides that fallback. Invalid quoting fails explicitly.
- Step env wins over runs.env defaults. INPUT_*, STATE_*, GITHUB_* and runtime
  service env use the normal environment builder. Image PATH remains the default
  unless the workflow explicitly supplies PATH; runner PATH additions are applied.
- Mount workspace, per-job shared home, workflow event directory, file commands,
  runner temp and /var/run/docker.sock at their /github destinations. Translate
  path-valued environment values and command paths without splitting space-bearing
  paths. Registration credentials and auth stores are never mounted.

## Failure modes and edge cases

Missing image/Dockerfile, malformed args, failed pull/build/create/start/attach/wait,
nonzero exit, and cleanup failure are visible step failures. A cancelled step is
Cancelled; a timeout is Failure. Owned containers are removed after success,
failure, timeout and cancellation; unrelated containers/networks survive. No
network is created or removed by the action adapter. Local actions skip pre with the upstream warning; remote pre failure prevents main.
A started main registers post even when main fails or is cancelled. Pre/post conditions use job context, without the action inputs context. Posts
use live job status, drain LIFO, and retain originating scoped state and resolved
inputs for their process environment and args.
Pre/post command outputs do not overwrite the main step output namespace.

## Acceptance criteria

- **AC-1:** A Docker action manifest retains every runs field and executes evaluated
  argv/env with exact empty/space-bearing values and documented precedence (75-S2).
- **AC-2:** Registry, repository Dockerfile and local Dockerfile actions execute
  real code; a repeated immutable SHA reuses an existing successful build (75-S1).
- **AC-3:** Containers access standard mounts and propagate output/env/PATH/state
  and workflow annotations through normal production command handlers (75-S3).
- **AC-4:** Pre/main/post execute conditionally in order with scoped state; posts
  drain LIFO and their failures affect final job result (75-S2).
- **AC-5:** Actions connect on the job network; pull/build/entrypoint failures,
  cancellation and timeout have expected conclusions and remove owned containers
  while preserving unrelated resources (75-S4).
- **AC-6:** macOS rejects Docker actions explicitly without running host code (75-S5).
- **AC-7:** The full repository gate passes and committed documentation maps every
  scenario to evidence, including live/reference/platform/backend applicability.

## Acceptance evidence

| AC | Real input and expected observation | Check / boundary |
| --- | --- | --- |
| 1 | Committed Docker probe manifest, argv containing empty, spaces, quotes and inputs; exact recorded argv/env | sibling manifest tests and Linux production replay; absent/empty/override cases |
| 2 | Sanitized #73 acquired envelope with documented Docker-step variations; real pinned image and committed local/remote probe | Linux replay executes markers; second immutable build retains image ID; missing image/build failure |
| 3 | Probe writes GITHUB_OUTPUT/ENV/PATH/STATE, emits annotation, checks event/workspace/home/socket in space-bearing root | Linux replay asserts subsequent consumer and post markers plus annotation event |
| 4 | Two probe instances, pre/post conditions, distinct saved state, failing post | Linux replay checks order, separate STATE, final Failure and subsequent eligible post |
| 5 | Real job network and peer container; real invalid image/Dockerfile/entrypoint; cancel after observed start | Linux replay checks DNS/TCP marker and Docker inspect 404 after each exit; timeout and unrelated survivor |
| 6 | Same acquired registry/local references on macOS | production Runner replay asserts Linux-only diagnostic, Failure, no marker |
| 7 | Same probe revisions on toolu and official 2.337.0 (cab9d1c); real acquired logs/annotation and completion | ./tools/check.sh all; live evidence checker; coverage-map review |

Production replay lives in crates/execution/tests/docker_action_test.rs and
crates/execution/tests/docker_action_linux_test.rs; manifest tests in the sibling
actions/tests directory. scripts/test/docker_actions_linux.sh runs the opt-in
Linux real-daemon lane and fails when prerequisites or required tests are absent.
.github/actions/docker-action-probe and .github/workflows/docker-actions-live.yml
supply the live/reference lane. Fixture provenance identifies deliberate variants
separately from acquired fields. Linux ARM64 is locally available; x64 and GHES
require external evidence. No ignored or missing test counts as a pass.

## Documentation impact

Update README.md, docs/architecture.md, docs/test-coverage.md and affected module
README tables. Keep durable design here under docs/specs, following this epic's
committed design precedent; transient skill/ledger state stays under docs/toolu.

## Open Questions

No undecided behavior. Live paired Linux runner and GHES availability are external
verification prerequisites owned by the epic orchestrator, to be checked before
claiming closure. Missing access is reported needs-human, never substituted by
local replay. Service-only cross-feature evidence awaits #74 integration on main.

## Decision evidence

Pinned upstream ContainerActionHandler.cs establishes Linux-only dispatch,
entrypoint/args fallback, runs.env as defaults, network and standard mounts.
#125 supplies JobContainer::network; #127 supplies scoped LIFO post completion.
Jev recommended reusing these boundaries (reuse 0.99) instead of implementing
#74 inside #75. Comemory lookup failed due to newer database schema; no recalled
memory was used.

## Spec review

Approved for planning: all authored sections and AC evidence reviewed against #75,
#67, #125/#127 and the pinned upstream handler. Jev alignment result 0.76 prompted
a direct check of missing live prerequisites: they block closure, not the defined
implementation. Every scenario remains required; no evidence waiver is inferred.

## Execution authorization update

The orchestrator confirmed the supported test routes: ./tools/check.sh all on
the host and focused package/replay tests inside Docker. Bare host cargo test
remains denied. Execution resumed with these routes; no deny-rule change is
needed. CI/ci-macos may supply authoritative host evidence if local Node stalls.

## Upstream lifecycle correction during execution review

Pinned `ActionManager.cs:372-401` registers pre only for remote repository actions;
`ActionRunner.cs:104-109` warns that local action pre is unsupported. Action
inputs are added only for args/env in `ContainerActionHandler.cs:125-168`, not
for pre/post conditions. The probe therefore uses job-context conditions and
local replay asserts the pre warning/skip. Remote pre requires a repository
action replay. This corrects an initial probe assumption, not the upstream
contract. Jev selected the upstream contract at 0.97 after this new evidence.
