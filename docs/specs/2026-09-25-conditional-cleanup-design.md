# Main-step errors and conditional cancellation — Design

**Date:** 2026-09-25   **Status:** Approved   **Author:** Codex   **Topic:** #100

## Problem

Main-step execution errors bypass result adjustment and later failure cleanup.
Job cancellation kills every running condition and returns before eligible main
cleanup. A shared token also hides runner shutdown from GitHub job cancellation.

## Non-Goals

1. Change #99's deferred timeout/continue-on-error token decoding.
2. Redesign #101's post result aggregation, #102's composite scopes, Docker service
   provisioning, or #89's cleanup of deliberately detached processes.
3. Claim live platform/backend parity from local replay or static upstream review.

## Architecture

Normalize errors owned by an executing step into Failure, record raw outcome,
apply continue-on-error, report one adjusted completion, and evaluate later
conditions. Invalid conditions and step-env evaluation fail their own step without continue-on-error
(the pinned runner handles these outside RunStepAsync). Job workspace, services,
container initialization and acquired job defaults remain fatal setup errors.

Carry independent job cancellation and runner shutdown signals into execution.
Existing execute_job callers keep graceful job-cancellation semantics; the listener
supplies its session token separately as shutdown. A job-scoped cancellation
controller starts one five-minute deadline when cancellation is observed; it is
shared by current execution, eligible main cleanup, and post cleanup. Shutdown
cancels current execution and skips later user steps, with final Failure, matching
pinned StepsRunner.cs. Job cancellation sets Cancelled, re-evaluates the current
condition with cancelled job status, and cancels that step only if false/error.
Later steps evaluate against the live cancelled context; cleanup success cannot
replace the cancelled job conclusion. Step timeout remains Failure.

Per-step cancellation must reach subprocess waits and action resolution without
dropping executing futures before they reap owned children. Unix shell/Node
children enter a dedicated process group at spawn; cancellation/timeout kills
that group and reaps the leader, leaving other jobs' groups alone. Processes
that intentionally escape their process group remain #89's responsibility.
Container teardown continues through the existing finish_container path.

Reference: actions/runner at cab9d1c3901e45c7705889c4f88284fdd93f4ae5,
src/Runner.Worker/StepsRunner.cs. Static source inspection establishes policy,
not reference execution evidence. Jev evaluated the supplied source/issue and
selected separate cancellation/shutdown signals over retaining one token.

## Interfaces / Schema

- Add Runner::execute_job_with_shutdown(job, job_cancel, shutdown) while preserving
  execute_job(job, job_cancel). Add the corresponding run_job entry point.
- ExecutionContext retains the shutdown signal; run_steps constructs a job-scoped
  cancellation controller with tokens and one shared deadline.
- JobCtx exposes that controller to main and post execution. A per-step monitor
  evaluates the step-start condition context with cancelled status, owns a fresh
  child cancellation token, and respects the shared deadline and shutdown.
- Wire UUIDs remain report identities; contextName remains the expression key.
- No configuration, message schema, CLI, or persisted state change.
- Remote fetches use the shutdown signal; each action caller remains bounded by its
  step token/deadline so a cancelled job does not poison later cleanup downloads.

## Failure modes and edge cases

- Nonzero exit, missing executable/cwd, local missing manifest, action resolution
  and handler errors: failure outcome, success conclusion only with continue-on-error;
  following default/failure/always/!cancelled/cancelled conditions see adjusted state.
- Malformed if: failed outcome/conclusion and visible UUID-attributed error;
  continue-on-error cannot turn a condition-evaluation failure into success.
- Empty or absent if uses success(). Already-cancelled jobs evaluate cleanup only.
- Cancellation before first step, between steps, during resolution, and during a
  timeout race: cancellation wins the job result; each started step completes once.
- Cancellation during always(): running work may finish inside remaining five-minute
  budget. success()/!cancelled() work is interrupted. No fresh budget per cleanup.
- Shutdown always interrupts, skips later conditions, returns Failure, and reaches
  resource teardown. Grace expiry stops eligible cleanup and keeps Cancelled.
- Reap is bounded by existing ten-second grace and pipe draining by two seconds
  per stream after process termination; Docker cleanup uses its existing bounds.
- Setup failure cannot safely enter user steps. Existing setup teardown remains
  responsible for partially acquired resources.

## Acceptance criteria

- **AC-1:** Real nonzero exits, missing shell/cwd, and action/manifest failures
  retain failure outcome; with continue-on-error they conclude success and permit
  ordinary work, otherwise failure cleanup runs and the job fails.
- **AC-2:** After failure/timeout the marker sequence is failure, always,
  not-cancelled; ordinary and cancelled are skipped. After GitHub cancellation,
  always and cancelled run and the job remains cancelled.
- **AC-3:** Observed-start cancellation interrupts success()/!cancelled() work,
  permits always() work to finish inside the shared bound, and differs from
  runner shutdown which interrupts all conditions and skips later user work.
- **AC-4:** Cancellation before/between steps and during action resolution or
  timeout stays within the documented bound, leaves no live owned process-group
  child/grandchild, and leaves an unrelated process alive.
- **AC-5:** Errors/cancellation produce one completion per started step with wire
  identity, preserve registered LIFO posts and resource teardown, and produce one
  aggregate job completion consistent with live GitHub reporting.

## Acceptance evidence

| AC / scenario | Input, expected observations and runnable evidence |
| --- | --- |
| AC-1 / S1 | Transform the sanitized #68 incoming_contexts_matrix_0.json capture's existing script/action token shapes into nonzero, missing cwd/shell and missing manifest probes; actual Bash/Node/actions. cargo test -p execution --test conditional_cleanup_test asserts raw/effective outputs, marker order and UUID attribution for both continue settings. |
| AC-2 / S2 | Same capture with five later conditions; exact sequences above, including timeout Failure. Same command. |
| AC-3 / S3 | Real shell writes an observed start marker and waits for a release marker; cancel only after start, compare running condition variants, then verify later cleanup and final result. Same command. |
| AC-4 / S4-S5 | Real subprocess family plus unrelated process; before-first, between-step and observed-start cancellation, action resolution and timeout boundaries. Same command plus cargo test -p execution step_timeout and existing job-container cancellation tests on Linux Docker. Actual CLI SIGTERM and equivalent pinned official runner comparison are required live lanes. |
| AC-5 / S1-S5 | Captured production Runner replay includes real Node post and checks completion cardinality and IDs; existing post_results_test and container teardown tests remain regressions. Real acquired GitHub.com/GHES jobs validate backend conclusions, logs, and owned resource cleanup. |

Every fixture transformation is explicit and preserves captured types and distinct
UUID/contextName. Commit a coverage matrix with exact test names and results.
Required full gate: ./tools/check.sh all. macOS ARM64 host replay and Linux/Docker
are distinct lanes. GitHub.com and supported GHES need actual acquired jobs;
reference comparisons use identical workflow/action revisions on equivalent hosts
and pinned official runner cab9d1c. Missing credentials, reference host, GHES, or
Docker lanes remain unverified and block acceptance closure, never passing by skip.

## Documentation impact

Update README.md cancellation/error semantics, docs/architecture.md control flow,
and docs/test-coverage.md with AC/scenario evidence and explicit unverified lanes.
The durable design and plan use tracked docs/specs and docs/plans, matching #102,
rather than ignored docs/toolu.

## Open Questions

None affecting implementation. The available repository runner inventory currently
contains only an offline macOS toolu-70 runner; reference/GHES availability must be
established before claiming live acceptance. The worker owns that investigation.

## Spec review

Approved after checking all authored sections against #100 and pinned upstream.
Review corrections: step-env evaluation is pre-execution failure; action fetcher
must not retain the graceful cancellation token for eligible cleanup downloads.
Orchestrator clarification (2026-09-25): unavailable live/reference lanes must
remain unverified with reasons, but do not block PR delivery. GHES is unavailable
for this epic run. Local tests do not establish full live parity.
