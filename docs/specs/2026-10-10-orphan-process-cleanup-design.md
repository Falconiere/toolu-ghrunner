# Orphan process cleanup (`RUNNER_TRACKING_ID`) — Design

**Date:** 2026-10-10   **Status:** Draft   **Author:** Claude   **Topic:** Issue #89, per-job process tracking and job-end orphan termination

## Problem

Steps routinely leave background processes behind: `nohup` daemons, dev servers,
`setsid`-detached helpers, and grandchildren that outlive their shell. Step
timeout and cancellation already `SIGKILL` the step's own process group, but a
process that leaves that group, or one that a step that exited normally leaves
running, survives the job. On a persistent self-hosted host it then leaks into
every later job: it holds ports and files, uses CPU, and can be reached by the
next job's code. The pinned official runner ([`JobExtension.cs:586-600`](https://github.com/actions/runner/blob/cab9d1c3901e45c7705889c4f88284fdd93f4ae5/src/Runner.Worker/JobExtension.cs#L586-L600),
[`880-925`](https://github.com/actions/runner/blob/cab9d1c3901e45c7705889c4f88284fdd93f4ae5/src/Runner.Worker/JobExtension.cs#L880-L925))
tags every step process with a per-job `RUNNER_TRACKING_ID=github_<guid>` and
kills every process still carrying it at "Complete job". toolu has no
equivalent.

## Non-Goals

1. Processes inside Docker containers (job containers, service containers,
   Docker actions, `docker run -d` from a step). Upstream sets the id only on
   the worker's own process environment, which `docker exec`/`docker run` do
   not inherit. Container teardown already owns those processes.
2. Processes that drop the variable before they `exec`: `sudo` (env_reset),
   `env -i`, `su -`, and daemons that sanitize their environment. Process
   environments of other users or setuid processes are also out of scope, since
   they cannot be read without privilege. All of this matches upstream and will
   be documented, not claimed.
3. Killing anything between steps. Background services started in one step
   and used by a later step must keep running until the job ends.
4. Windows. toolu does not support Windows hosts.

## Architecture

**Tracking id.** At job entry, after the context is built,
`ExecutionContext` receives an `Option<ProcessTracking>`. When the job message
variable `process.clean` parses as boolean `false`, the value is `None`. As in
upstream `GetBoolean`, the parse is case-insensitive `true`/`false`, and any
other value, or no value, means enabled. Otherwise it holds the id
`github_<uuid v4>`, which is fresh for every job and never reused.

**Injection as a host spawn flag.** This was chosen over a job-global env entry
(Jev: `spawn_flag` 0.95 versus `global_env` 0.01). Upstream sets the variable
on its *worker process* environment. Children inherit it, any workflow layer
(job `env`, step `env`, `GITHUB_ENV`) overrides it, it is absent from
`${{ env }}`, and container execs never receive it. toolu cannot change its own
environment: the process is multithreaded and `unsafe_code` is forbidden. So
the same contract is reproduced at the spawn boundary:

- `context::safe_process_env_vars()` also strips `RUNNER_TRACKING_ID`, so the
  value toolu inherits from an outer runner never reaches a step. This mirrors
  upstream's `SetEnvironmentVariable`, which overwrites the inherited value.
- `step_process_env::apply(env, container_ci, tracking_id)` inserts the id with
  `entry().or_insert`, so any workflow-layer value wins, including an empty
  string. That empty string is upstream's documented opt-out. Host shells
  (`ScriptHandler`, which serves `run:` steps, composite shells and job hooks)
  and host Node stages (pre, main and post) pass `ctx.process_tracking_id()`,
  which travels beside the existing `cgroup_path` in `ScriptParams` /
  `NodeExecParams`. Container exec paths and Docker actions pass `None`.

**Job-end cleanup.** A new module, `execution::orphan_cleanup`, runs once per
job in `job_runner::entry::finish_execution`. The call comes after
`finish_container` (containers and services are already removed, as in
upstream's ordering, where "Stop containers" precedes "Complete job"). It comes
before the result branches, so it runs for a successful body, a failed body, a
cancelled body, an errored body and a container-teardown error, and always
before `JobCompleted` is emitted. No step process can exist before the body
starts: the job-started hook is the first spawn and it runs inside the body. So
the earlier setup error returns need no cleanup. Steps:

1. Emit the job-level log line `Cleaning up orphan processes`, using
   `step_id: ""` as service-container teardown logs do.
2. On a blocking thread, enumerate processes with `sysinfo` (already a
   workspace dependency, `0.37`). Read only the environment, with
   `ProcessRefreshKind::nothing().with_environ(UpdateKind::Always)`. On Linux
   that reads `/proc/<pid>/environ`; on macOS it reads `sysctl(KERN_PROCARGS2)`.
   Upstream's macOS path scans `ps e` output for a substring, which this
   replaces with real parsing. Skip the runner's own pid and thread entries. A
   candidate is a process whose environment has an entry named exactly
   `RUNNER_TRACKING_ID` whose value equals the id under ASCII case-insensitive
   comparison (upstream `OrdinalIgnoreCase`). When an environment cannot be
   read, that process is skipped, never matched.
3. For each candidate, refresh that one pid. Re-confirm that its start time is
   unchanged and that it still carries the id, then send `SIGKILL`, which is
   what upstream's `Process.Kill()` sends (Jev: `sigkill` 0.97). Log
   `Terminate orphan process: pid (<pid>) (<name>)`, which is upstream's
   wording.
4. Wait up to `ORPHAN_EXIT_WAIT` (5 s), polling every 100 ms, until each killed
   pid has gone or is a zombie. Survivors get one WARN log line naming them.
   The whole scan, kill and wait is bounded by `ORPHAN_CLEANUP_TIMEOUT` (15 s).
   On timeout, a WARN line is logged and the job finishes anyway.

There is no start-of-job process snapshot (Jev: `id_only` 0.77 versus
`replicate` 0.22). Upstream excludes pre-existing `pid_name` pairs, but the id
is fresh per job, so no pre-existing process can carry it. A snapshot would
only add a PID-reuse false *negative*. The start-time recheck in step 3 is the
guard against PID reuse.

Cleanup never changes the job conclusion and never fails the job. An
enumeration panic or a join error is converted to a WARN line.

## Interfaces / Schema

- `execution::orphan_cleanup::ProcessTracking { id: String }` (crate-public):
  - `ProcessTracking::for_job(variables: &HashMap<String, VariableValue>) -> Option<Self>`
  - `ProcessTracking::id(&self) -> &str`
  - `async fn cleanup(&self, events: &mpsc::Sender<RunnerEvent>) -> CleanupReport`
  - `CleanupReport { terminated: Vec<u32>, survivors: Vec<u32>, timed_out: bool }`
    (for logs and tests).
  - Constants: `TRACKING_ENV = "RUNNER_TRACKING_ID"`,
    `PROCESS_CLEAN_VARIABLE = "process.clean"`, `ORPHAN_EXIT_WAIT`,
    `ORPHAN_CLEANUP_TIMEOUT`.
- `ExecutionContext::{set_process_tracking, process_tracking, process_tracking_id}`.
- `step_process_env::apply(&mut HashMap<String,String>, Option<&str> /*container CI*/, Option<&str> /*tracking id*/)`.
- New `tracking_id: Option<&'a str>` field on `ScriptParams` and
  `NodeExecParams` (and `ShellScriptParams`, which forwards to `ScriptParams`).
- New dependency: `sysinfo = "0.37"` in `crates/execution/Cargo.toml`, the same
  version the `config` crate pins. There are no wire, config-file or CLI
  changes.
- User-visible: step children see `RUNNER_TRACKING_ID=github_<uuid>`. Job log
  lines read `Cleaning up orphan processes` and
  `Terminate orphan process: pid (N) (name)`.

## Failure modes and edge cases

- **Opt-out.** A step, job or `GITHUB_ENV` value for `RUNNER_TRACKING_ID`,
  including the empty string, is kept unchanged. Processes started under it
  survive cleanup. `process.clean=false` sets no variable and skips cleanup
  entirely, with no log line, as upstream does.
- **Inherited outer id.** When toolu itself runs under another runner, which
  sets `RUNNER_TRACKING_ID=github_outer` on toolu's process, steps still
  receive this job's id. The outer value never matches this job's cleanup, and
  toolu's own process is skipped.
- **Concurrent jobs.** Each toolu process for each repository has its own ids.
  Job A's cleanup does not touch job B's live processes, and it does not touch
  any process without an id.
- **Unreadable environment** (another user, setuid, a process that exits during
  the scan, a macOS `sysctl` failure): skipped. These are never treated as a
  match.
- **PID reuse** between the scan and the kill: if the start time changed or the
  id is gone, there is no kill. The remaining window between the recheck and
  `kill(2)` is microseconds. Reuse inside it would need the kernel to cycle
  through the whole PID space, so it is documented as a residual risk rather
  than claimed impossible. Neither Linux pidfd nor any lock-free alternative is
  available without `unsafe`.
- **Process holding the step's stdout open.** The existing `bounded_drain`
  (2 s) stops the step from blocking on it. The holder is then killed at job
  end, so log draining cannot hang.
- **Zombies.** Orphans are re-parented to init or a subreaper, which reaps them.
  A zombie counts as terminated. In `boot` mode toolu is PID 1, so killed
  orphans stay zombies until the container exits. The container exits right
  after the single job, so this is documented, not fixed.
- **Cancellation or shutdown during cleanup.** Cleanup does not watch the
  cancel or shutdown tokens. Instead it is bounded by `ORPHAN_CLEANUP_TIMEOUT`,
  so a cancelled job still cleans up and cannot hang.
- **Setup failure** (a job-started hook exits non-zero), **step failure**,
  **failing Node post** and **job cancellation** all reach `finish_execution`.
  Each one cleans up.

## Acceptance criteria

- **AC-1:** Given a captured GitHub job replay, a host `run:` step, a Node
  action stage and a job hook each print
  `RUNNER_TRACKING_ID=github_<uuid v4>`. The value is identical within one job,
  differs between two jobs, and is never an inherited outer value.
- **AC-2:** Given a replayed job whose step leaves a background child holding
  stdout, a grandchild, a `nohup` process and a `setsid`-detached process, every
  one of them is gone or a zombie by the time `run_job` returns. This holds for
  a successful job, a failed job and a cancelled job. The job log contains
  `Cleaning up orphan processes` and one `Terminate orphan process: pid (<pid>)`
  line per process. The job finishes within the documented bound
  (step + 2 s drain + 15 s cleanup).
- **AC-3:** Given two concurrent replayed jobs and an unrelated process with no
  tracking id, the first job's cleanup leaves the second job's detached process
  and the unrelated process alive. The second job's own cleanup later kills its
  process.
- **AC-4:** Given a step with `env: RUNNER_TRACKING_ID: ""`, its detached
  process survives cleanup. Given `process.clean=false`, steps see no
  `RUNNER_TRACKING_ID`, no cleanup log line is emitted, and a detached process
  survives.
- **AC-5:** A candidate whose recorded start time no longer matches the live
  process (simulated PID reuse) is not signalled, and the real process stays
  alive.
- **AC-6:** A job-started hook that detaches a process and exits 1 (setup
  failure), and a Node action whose post stage detaches a process and fails,
  both still get that process killed. The job conclusion stays `Failure`.
- **AC-7:** `./tools/check.sh all` passes. The README, `docs/test-coverage.md`
  and `docs/architecture.md` describe the behavior, the opt-outs, the Linux
  and macOS mechanisms and limits, and which lanes are verified and which are
  unverified.

## Acceptance evidence

| AC / scenario | Real input → exact observable result | Runnable proof / applicability |
| --- | --- | --- |
| AC-1 / issue AC 1 | Sanitized GitHub.com capture `crates/execution/tests/incoming_contexts_matrix_0.json` (`__run` step), the committed local Node action `orphan-89-node`, and a real `ACTIONS_RUNNER_HOOK_JOB_STARTED` script. Each prints `TRACK89\|<stage>\|<value>`; values match `^github_[0-9a-f-]{36}$`, are equal within the job and differ across two jobs. A self re-exec of the test binary with `RUNNER_TRACKING_ID=github_outer89` in its env proves the inherited value is replaced. | `cargo test -p execution --test orphan_cleanup_test`, real Bash and Node, Linux (local + CI `ubuntu-latest`) and macOS (CI `macos-14`). |
| AC-2 / 89-S1 | The step script starts `sleep` with stdout inherited, a `bash -c` grandchild, `nohup sleep` and a `python3` `os.setsid()` sleeper. It writes their pids and then ends with success, `exit 1`, or blocks until the test cancels the token. After the event stream closes, `ps -o stat= -p <pid>` prints nothing or `Z…` for each pid. The log lines are present and the elapsed time is below the bound. | Same test file, three conclusions. Linux + macOS CI. |
| AC-3 / 89-S2 | Two jobs run concurrently on one tokio runtime. Job B waits on a marker file that the test writes only after job A has finished. The test's own `sleep` has no id. After A finishes, B's pid and the unrelated pid are alive (`ps` shows a non-zombie state). After B finishes, B's pid is gone. | Same test file. |
| AC-4 / 89-S3 | Step `environment` token `RUNNER_TRACKING_ID: ''`, and job variable `process.clean=false`: the detached pid stays alive after the job (the test then kills it), the step prints empty or unset, and the `process.clean` case logs no cleanup line. The macOS limits (KERN_PROCARGS2 same-user only, env captured at exec) and the `sudo`/`env -i` escapes are documented, not tested as guarantees. | Same test file; docs review. |
| AC-5 / 89-S2 PID reuse | A real spawned `sleep`; the identity recheck runs with a deliberately wrong start time and must return "skip". `kill -0` still succeeds afterwards. | Sibling unit test `crates/execution/src/execution/tests/orphan_cleanup.rs` (`cargo test -p execution orphan_cleanup`). |
| AC-6 / 89-S4 | The job-started hook script detaches a `python3` sleeper and exits 1. The local Node action `orphan-89-node` has a post stage that spawns `detached: true` `sleep` and exits 1. Both pids are dead after the job, and the conclusion is `Failure`. Cancellation during cleanup is covered by the cancelled case in AC-2, where cleanup runs after the token has fired. | `orphan_cleanup_test`; Linux + macOS CI. |
| AC-7 / gate + docs | Gate exits 0. For the reference lane, `.github/workflows/orphan-cleanup-89.yml` runs the same detach patterns on GitHub-hosted `ubuntu-24.04` and `macos-15`, which use the official runner, and links the run's "Complete job" `Terminate orphan process` lines. A toolu self-hosted lane is gated behind `workflow_dispatch` `run_toolu`. If no toolu runner is registered, that lane is recorded as **unverified**. GHES uses the same process rule but stays **unverified** without a GHES acquisition. | `./tools/check.sh all`; workflow run link recorded in `docs/test-coverage.md`. |

## Documentation impact

- `README.md`: in "What runs", next to the CI/GITHUB_ACTIONS paragraph, a
  paragraph on `RUNNER_TRACKING_ID`, job-end termination, the opt-outs and the
  limits. In "Step failure and cancellation cleanup", replace "processes that
  deliberately detach … remain outside this guarantee" with a reference to
  job-end tracking.
- `docs/test-coverage.md`: an "Issue #89 — orphan process cleanup" section
  mapping every AC and scenario to a test, command, platform, and
  verified/unverified status.
- `docs/architecture.md`: one line on the job-end orphan sweep in the job
  lifecycle.
- `AGENTS.md`: list `orphan_cleanup` among the execution modules.

## Open Questions

None blocking. The decisions above (spawn-flag injection, no snapshot, SIGKILL,
`sysinfo`) come from the issue, the upstream contract pinned by #67, and Jev
checks over that evidence. Live toolu self-hosted and GHES lanes stay
unverified unless a registered runner is available. The issue's acceptance
rules record that as unverified rather than failing.
