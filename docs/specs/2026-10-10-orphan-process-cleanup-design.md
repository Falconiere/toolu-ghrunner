# Orphan process cleanup (`RUNNER_TRACKING_ID`) — Design

**Date:** 2026-10-10   **Status:** Approved (rev 3)   **Author:** Claude   **Topic:** Issue #89, per-job process tracking and job-end orphan termination

## Problem

Steps routinely leave background processes behind: `nohup` daemons, dev
servers, `setsid`-detached helpers, and grandchildren that outlive their shell.
Step timeout and cancellation already `SIGKILL` the step's own process group.
That does not reach a process that leaves the group, or anything still running
after a step exits normally, so such processes survive the job. On a persistent
self-hosted host they then leak into every later job. They hold ports and
files, use CPU, and can be reached by the next job's code.

The pinned official runner tags every step process with a per-job
`RUNNER_TRACKING_ID=github_<guid>` and kills every process still carrying it at
"Complete job"
([`JobExtension.cs:586-600`](https://github.com/actions/runner/blob/cab9d1c3901e45c7705889c4f88284fdd93f4ae5/src/Runner.Worker/JobExtension.cs#L586-L600),
[`880-925`](https://github.com/actions/runner/blob/cab9d1c3901e45c7705889c4f88284fdd93f4ae5/src/Runner.Worker/JobExtension.cs#L880-L925)).
toolu has no equivalent.

## Non-Goals

1. **Processes inside Docker containers.** This covers job containers, service
   containers, Docker actions, and `docker run -d` from a step. Upstream sets
   the id only on the worker's own process environment, which
   `docker exec`/`docker run` do not inherit, and container teardown already
   owns those processes.
2. **Processes that drop the variable before they `exec`.** Examples are
   `sudo` with env_reset, `env -i`, `su -`, and daemons that sanitize their
   environment. Processes whose environment cannot be read (other users,
   setuid binaries) are also excluded. Both limits match upstream and are
   documented, not claimed as covered.
3. **Killing anything between steps.** Background services started in one step
   and used by a later step keep running until the job ends.
4. **Paths that never reach job finalization.** This covers a runner crash, an
   external `SIGKILL` of the runner, a panic, and the `boot` deadline
   watchdog's hard `exit(124)` (`boot_cmd.rs`, 30 s grace). On those paths the
   orphans survive, exactly as upstream's do when its worker dies.
5. **Windows.** toolu does not support Windows hosts.

## Architecture

### Tracking id

At job entry, right after the context is built, `ExecutionContext` receives an
`Option<ProcessTracking>`:

- `process.clean` is looked up in `msg.variables` with an ASCII
  case-insensitive key match.
- If the trimmed value parses case-insensitively as `false`, the result is
  `None`. This mirrors upstream `GetBoolean`, which uses .NET
  `bool.TryParse` (trim, `true`/`false` only, any case).
- Any other value, including `0`, a missing variable, or garbage, means
  tracking is enabled.
- When enabled, the id is `github_<uuid v4>`. It is fresh for every job and
  never reused.

### Injection: a spawn-time host flag, not job-global env

Upstream sets the id on its **worker process** environment. That has four
consequences:

- children inherit it;
- any workflow layer (job `env`, step `env`, `GITHUB_ENV`) overrides it;
- it is absent from `${{ env }}`;
- container execs never receive it.

toolu cannot change its own environment, because the process is multithreaded
and `unsafe_code` is forbidden. Putting the id in `ExecutionContext.env`
instead would make it appear in `${{ env }}` and in `docker exec` env, both of
which upstream excludes. So the contract is reproduced at the spawn boundary:

- `context::safe_process_env_vars()` strips `RUNNER_TRACKING_ID`. It is the
  only fold of runner process env into step env, used by
  `script_support.rs:87`, `composite_env.rs:100`, `job_hooks.rs:162` and
  `node_exec.rs:147`. A value inherited from an outer runner therefore never
  passes through the generic fold.
- `ExecutionContext::process_tracking_env() -> Option<String>` returns this
  job's id when tracking is enabled. When `process.clean=false`, it returns the
  inherited value read through `crate::config::var("RUNNER_TRACKING_ID")`,
  which preserves upstream visibility in that mode.
- `step_process_env::apply(env, container_ci, tracking)` inserts `tracking`
  with `entry().or_insert`. Any workflow-layer value therefore wins, including
  an empty string, which is upstream's documented opt-out.
- Host spawns pass `Some`:
  - `ScriptHandler` (`script.rs:231`) serves `run:` steps, composite shells and
    job hooks.
  - Host Node stages pre/main/post go through `node_exec.rs:138`.

  The value arrives through a new `tracking_id` field placed beside
  `cgroup_path`.
- Container exec (`container_exec.rs:127`, `job_container.rs:159`) and Docker
  actions (`action_container.rs:305`) pass `None`.

### Job-end cleanup

`execution::orphan_cleanup` runs once per job inside
`job_runner::entry::finish_execution`, which is restructured as follows:

```text
let finished = finish_container(ctx, body_result, events).await;   // containers/services gone first
sweep_orphans(ctx, events).await;                                    // always, before branching
match finished { Ok(..) => finish_job(..) /* emits JobCompleted */, Err(e) => { stop_local_services; Err(e) } }
```

**Ordering.** The sweep runs for every body outcome: success, failure,
cancellation and error. It also runs after a container-teardown error, and
always before `JobCompleted`. This matches upstream, where "Stop containers"
runs before "Complete job". The sweep runs for container jobs too, because
host job hooks can carry the id. Upstream does not skip it either.

**Where it does not run, and why that is safe.** No step process can exist
before the body starts, because the job-started hook is the first spawn and it
lives inside the body. So these paths need no sweep:

- the setup `?` returns in `entry::run`;
- `ContainerStart::Finished`, when cancellation hits during container start;
- the setup `Err` from `start_job_container`.

**Steps.**

1. **Log the start.** Emit the job-level log line `Cleaning up orphan processes`
   with `step_id: ""`, the convention `docker/services.rs` uses. If
   `events.send` fails, log a WARN and continue.
2. **Scan.** On a blocking thread, use `sysinfo 0.37` (already pinned by
   `config`):
   - Call `System::refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing().without_tasks().with_environ(UpdateKind::Always))`.
   - `without_tasks()` stops Linux from reading the environment of every
     thread. `thread_kind().is_some()` entries are still skipped as a guard.
   - Linux reads `/proc/<pid>/environ`. macOS reads
     `sysctl(KERN_PROCARGS2)`, which is proper parsing in place of upstream's
     substring scan of `ps e` output.
   - A candidate is any process other than the runner's own pid whose
     environment contains a `RUNNER_TRACKING_ID` entry with that exact name and
     a value equal to the id under ASCII case-insensitive comparison. This
     matches upstream `OrdinalIgnoreCase`.
   - An empty or unreadable environment (another uid, a zombie, a race with
     exit) never matches.
   - If the runner's own pid is missing from the scan (for example `/proc`
     not mounted or unreadable, or sysctl denied), the scan is degraded.
     Report `degraded = true` and log one WARN instead of silently doing
     nothing. A legitimately empty runner environment (`env -i toolu-runner`)
     is not degraded.
3. **Kill.** For each candidate, refresh just that pid
   (`ProcessesToUpdate::Some`) and check that it **still carries this job's
   id**. That recheck is the PID-reuse guard: a reused pid that carries the
   id is, by definition, a process from this job. The start time (one-second
   resolution) is a coarse secondary check: if it differs, skip. Then call
   `kill_with(Signal::Kill)`, the SIGKILL that upstream `Process.Kill()`
   sends.
   - `Some(true)` logs `Terminate orphan process: pid (<pid>) (<name>)`,
     upstream's wording, and the pid joins the exit wait.
   - `Some(false)` or `None` records the pid as a survivor. A survivor whose
     pid has disappeared by the final wait poll (for example ESRCH, because it
     exited on its own) is moved to `terminated`.
4. **Re-scan.** Repeat steps 2–3, excluding pids already signalled or already
   recorded as survivors, until a pass yields no *new* candidate. This catches
   a supervisor that forks a replacement before its kill lands, and children
   spawned during the scan, without re-finding a killed-but-not-yet-exited pid
   and spinning to the deadline.
5. **Wait for exit.** Poll every 100 ms until each killed pid has gone or is a
   zombie (`ProcessStatus::Zombie`).

**Bounds.**

- All loops share one `deadline = start + ORPHAN_CLEANUP_TIMEOUT` (15 s),
  checked before every scan, kill and poll. When it passes, the blocking work
  stops and sets `timed_out`. A single `refresh_processes_specifics(All)` call
  cannot be interrupted, so the deadline is checked between scans, kills and
  polls; it is not a hard cap on one scan.
- The async side wraps the join in `tokio::time::timeout(ORPHAN_CLEANUP_TIMEOUT + 2 s)`.
  This is only a liveness backstop, for example against a `/proc` read blocked
  on a D-state process. The deadline is what actually stops the work.
- Survivors, a timeout and a degraded scan each produce one WARN log line.
  Cleanup never changes the job conclusion and never fails the job. A join
  error is logged as a WARN.

**No start-of-job process snapshot.** Upstream excludes processes whose
`pid_name` existed at job start. That exclusion protects reused ids, and the id
here is a fresh v4 UUID per job, so no pre-existing process can carry it. A
snapshot would only add a PID-reuse false negative.

**Signal.** SIGKILL with no SIGTERM grace, for parity with upstream's
`Process.Kill()`.

## Interfaces / Schema

- **New file `crates/execution/src/execution/orphan_cleanup.rs`**:
  - `ProcessTracking { id: String }` (crate-public), with
    `for_job(&HashMap<String, VariableValue>) -> Option<Self>` and
    `id(&self) -> &str`.
  - Pure helpers `process_clean_enabled(&HashMap<String, VariableValue>) -> bool`
    and `carries_id(environ: &[OsString], id: &str) -> bool`.
  - `pub(crate) async fn sweep(tracking: &ProcessTracking, events: &mpsc::Sender<RunnerEvent>) -> CleanupReport`.
  - Blocking core
    `sweep_blocking(id: &str, own_pid: u32, deadline: Instant) -> CleanupReport`.
    It has two-phase internals, `scan_candidates` and `kill_verified`, which
    the sibling unit tests call against real processes.
  - `CleanupReport { terminated: Vec<u32>, survivors: Vec<u32>, timed_out: bool, degraded: bool }`.
  - Constants: `TRACKING_ENV = "RUNNER_TRACKING_ID"`,
    `PROCESS_CLEAN_VARIABLE = "process.clean"`,
    `ORPHAN_CLEANUP_TIMEOUT = 15 s`, `EXIT_POLL = 100 ms`.
- **`ExecutionContext`**:
  - a new `process_tracking: Option<ProcessTracking>` field;
  - accessors `set_process_tracking`, `process_tracking`,
    `process_tracking_env` (in a new `context_process.rs` impl file, because
    `context.rs` has 395 of its 500 code lines used).
- **`step_process_env::apply(&mut HashMap<String,String>, container_ci: Option<&str>, tracking: Option<&str>)`**:
  - host call sites `script.rs:231` and `node_exec.rs:138` pass the param
    value;
  - container call sites `container_exec.rs:127`, `job_container.rs:159` and
    `action_container.rs:305` pass `None`.
- **New `pub tracking_id: Option<&'a str>` field** on `ScriptParams` and
  `NodeExecParams`, plus `ShellScriptParams` in `composite_shell.rs`, which
  forwards it. Every literal constructor sets it:
  - `steps_runner.rs:454`
  - `job_hooks.rs:100`
  - `composite_exec.rs:370` and `:442`
  - `composite_shell.rs:43`
  - `node_stage.rs:95`
  - `crates/execution/tests/shell_templates_test.rs:27` and `:105` (both pass
    `None`).
- **`config.rs` / `config::var`**: the inherited `RUNNER_TRACKING_ID` is read
  only through `crate::config::var`, as the `no-direct-env-var` rule requires.
- **Dependencies**: `sysinfo = "0.37"` added to `crates/execution/Cargo.toml`;
  `Cargo.lock` changes.
- **Unchanged**: no wire, config-file or CLI changes.
- **User-visible**:
  - steps see `RUNNER_TRACKING_ID=github_<uuid>`;
  - job log lines read `Cleaning up orphan processes`,
    `Terminate orphan process: pid (N) (name)`, and WARN lines for survivors,
    timeout or a degraded scan.

## Failure modes and edge cases

| Input / condition | Observable behavior | Propagation |
| --- | --- | --- |
| Step/job/`GITHUB_ENV` sets `RUNNER_TRACKING_ID` (incl. `''`) | Kept verbatim; processes started under it survive | none |
| Step re-exports the id upper-cased | Still matched (case-insensitive), killed | none |
| `process.clean` = `false`/`False`/` FALSE ` | No new id; inherited value (if any) passes through; no sweep, no log | none |
| `process.clean` = `0`, `no`, garbage, absent | Tracking enabled | none |
| Inherited outer id while enabled | Steps see this job's id; outer value never matched; runner's own pid skipped | none |
| Concurrent job (other toolu process) | Its processes carry a different id → untouched | none |
| Process without the id (unrelated) | Untouched | none |
| Unreadable/empty environ (other uid, zombie, exited mid-scan) | Never a candidate | none |
| Own pid missing from the scan | `degraded`, one WARN line | none |
| Candidate exec'd without the id, or start time changed, before kill | Not signalled | none |
| `kill` fails (EPERM, or ESRCH because it already exited) | EPERM: survivor + WARN; already-gone pid moved to `terminated` at the final poll | none |
| Forking supervisor / child spawned mid-scan | Re-scan pass catches it within the deadline | none |
| Deadline (15 s) exceeded / blocking join stalls | Work stops at deadline; `timed_out`; WARN; job finishes | none |
| Event channel closed | WARN, continue | none |
| Cancellation/shutdown fired during cleanup | Sweep does not watch tokens; completes within its deadline | none |
| Process holding the step's stdout | Existing 2 s `bounded_drain` unblocks the step; killed at job end | none |
| `boot` mode (toolu is PID 1) | Killed orphans may remain zombies until the container exits right after | documented |
| Setup failure (job-started hook exits 1), step failure, failing Node post, cancellation, body `Err` | All reach `finish_execution` → swept | conclusion unchanged |

**Worst-case bound per job end**: 15 s for cleanup, plus up to 2 s backstop.
This is in addition to the existing step-level costs of 10 s `REAP_GRACE` and
2 s pipe drain per step.

## Acceptance criteria

- **AC-1:** Given a replayed captured GitHub job, a host `run:` step, a host
  Node action stage and a job-started hook each print
  `RUNNER_TRACKING_ID=github_<uuid v4>`. The value is identical within one job
  and differs between two jobs. A test-binary re-exec whose environment
  carries `RUNNER_TRACKING_ID=github_outer89` still prints this job's id, and
  prints `github_outer89` when `process.clean=false`.
- **AC-2:** Given a replayed job whose step leaves four processes behind:
  - a background child holding stdout;
  - a `bash -c` grandchild;
  - a `nohup` process;
  - a `setsid`-detached process (`python3 os.setsid`).

  Every one of them is gone or a zombie when the job's event stream closes.
  This holds for success (`exit 0`), failure (`exit 1`) and cancellation (the
  token fires while the step blocks). The job log contains
  `Cleaning up orphan processes` and a `Terminate orphan process: pid (<pid>)`
  line for each one. A job with no orphans logs the header and no `Terminate`
  line.
- **AC-3:** Given two concurrently replayed jobs and an unrelated `sleep` with
  no id, job A's cleanup leaves job B's detached process and the unrelated
  process alive (not zombies). Job B's own cleanup then kills its process.
- **AC-4:** Given a step with `env: RUNNER_TRACKING_ID: ''`, its detached
  process survives cleanup. Given a step that re-exports the id upper-cased,
  its detached process is killed. Given `process.clean=false`, steps see no
  `RUNNER_TRACKING_ID` (no outer value), no cleanup line is logged, and the
  detached process survives. The pure parser treats `False` and ` FALSE ` as
  disabled, and `0`, `no` and `garbage` as enabled.
- **AC-5:** Given a real process that carried the id when scanned and then
  `exec`s `env -i sleep` before the kill phase (same pid, environment without
  the id, the observable stand-in for PID reuse), it is not signalled and stays
  alive. Given a real unreaped zombie child that carried the id, the sweep
  skips it without error. Given an already-passed deadline, the sweep returns
  `timed_out` and kills nothing.
- **AC-6:** Given a job-started hook that detaches a process and exits 1
  (setup failure), and a local Node action whose post stage spawns a
  `detached: true` process and exits 1, both processes are killed and the
  conclusion is `Failure`. Given cancellation and shutdown fired after the
  `Cleaning up orphan processes` line appears (that is, during cleanup), the
  orphan is still killed and the stream still closes.
- **AC-7:** Given a Linux job-container replay with real Docker, the container
  step prints `RUNNER_TRACKING_ID` unset, and the host sweep still runs
  without touching the container.
- **AC-8:** `./tools/check.sh all` passes. The README, `docs/test-coverage.md`,
  `docs/architecture.md`, `AGENTS.md`, `crates/execution/src/execution/README.md`
  and `crates/execution/src/execution/tests/README.md` describe the behavior,
  the opt-outs, the Linux and macOS mechanisms and limits, and which lanes are
  verified and which are unverified.

## Acceptance evidence

All replay tests use the sanitized GitHub.com capture
`crates/execution/tests/incoming_contexts_matrix_0.json`. They keep only its
`__run` steps (`context_name == "__run"`) and set the script token. The
`process.clean` and `environment` tokens are injected into the captured
message the same way `step_process_ci_test.rs` does. Liveness is observed with
real `ps -o stat= -p <pid>`: empty output or a leading `Z` means dead.

| AC / scenario | Real input → exact observable result | Runnable proof / platforms |
| --- | --- | --- |
| AC-1 / issue AC 1 | `TRACK89\|<stage>\|<value>` markers from Bash, Node (flat fixture files `orphan_89_action.yml`, `orphan_89_main.js`, `orphan_89_post.js` in `crates/execution/tests/`, installed under `.github/actions/orphan-89` like `post_results_test.rs`), and a real hook script. Regex `^github_[0-9a-f]{8}-…$`; equal within a job, different across jobs; re-exec child with `RUNNER_TRACKING_ID=github_outer89` asserts the replacement and the `process.clean=false` passthrough. | `cargo test -p execution --test orphan_cleanup_test`; Linux local + CI `ubuntu-latest`, macOS CI `macos-14` |
| AC-2 / 89-S1 | The four processes above; pid files; three conclusions; log lines; a no-orphan job | same |
| AC-3 / 89-S2 | Jobs A and B on one runtime; A's step blocks until the test has seen B's pid file (B's detached process exists before A's sweep), B waits on a marker the test writes after A's stream closes; unrelated `sleep` spawned by the test | same |
| AC-4 / 89-S3 | Step `environment` token `RUNNER_TRACKING_ID: ''`; upper-cased re-export; `process.clean=false` variable; parser table | same + sibling unit test `crates/execution/src/execution/tests/orphan_cleanup.rs` |
| AC-5 / 89-S2 lookup failure | `sh -c 'read _; exec env -i sleep 300'` carrying the id: `scan_candidates` finds it, the test releases the exec, `kill_verified` skips, and `kill -0` still succeeds. An unreaped `Child` that exited gives a real zombie. Deadline `Instant::now()` gives `timed_out`. | `cargo test -p execution orphan_cleanup` (sibling unit tests, real processes, no mocks) |
| AC-6 / 89-S4 | Real `ACTIONS_RUNNER_HOOK_JOB_STARTED` script exiting 1 after detaching; the Node fixture's post detaching a child and exiting 1; cancel and shutdown fired upon seeing the cleanup header | `orphan_cleanup_test` |
| AC-7 / Non-Goal 1 | Captured `crates/toolu-runner/tests/fixtures/job_container_message.json` with real Docker: the container step prints `TRACK89\|container\|unset` | `cargo test -p execution --test orphan_cleanup_container_test -- --ignored` (Linux + Docker; macOS not applicable). Ignored by default, so `./tools/check.sh all` does not run it; `docs/test-coverage.md` records it as a manual lane with command, platform and observed result, unverified-in-gate. |
| AC-8 / gate + docs | Gate exits 0. The reference lane `.github/workflows/orphan-cleanup-89.yml` follows the `step-process-ci-72.yml` precedent: the same detach patterns on GitHub-hosted `ubuntu-24.04` and `macos-15` (official runner), plus a `workflow_dispatch` `run_toolu` self-hosted lane. Its run link is evidence only and is not reproduced by the gate. A missing toolu runner or a missing GHES acquisition is recorded as **unverified**. | `./tools/check.sh all`; run link in `docs/test-coverage.md` |

## Documentation impact

- **`README.md`**:
  - In "What runs", add a paragraph next to the CI/GITHUB_ACTIONS paragraph
    covering the id, job-end termination, the opt-outs and the limits.
  - In "Step failure and cancellation cleanup", replace the clause saying
    detached processes "remain outside this guarantee" with a pointer to
    job-end tracking.
- **`docs/test-coverage.md`**: add a section "Issue #89 — orphan process
  cleanup" that maps each AC and scenario to its test, command, platform and
  verified/unverified status.
- **`docs/architecture.md`**: add one line on the job-end orphan sweep.
- **`AGENTS.md`**: add `orphan_cleanup` to the execution modules list.
- **`crates/execution/src/execution/README.md`**: add a row for
  `orphan_cleanup.rs` and one for `context_process.rs`, and update the
  `step_process_env.rs` row.
- **`crates/execution/src/execution/tests/README.md`**: add a row for
  `orphan_cleanup.rs`.

## Open Questions

None blocking. Every decision above is grounded in code or upstream:

- **Spawn-flag injection.** `${{ env }}` and `docker exec` isolation, and the
  `env_clear().envs()` in `script.rs`.
- **Passthrough when disabled.** Upstream never sets the variable in that mode.
- **No snapshot.** Ids are fresh v4 UUIDs.
- **SIGKILL.** Upstream uses `Process.Kill()`.
- **`sysinfo`.** It is already pinned in the workspace and supports macOS
  `KERN_PROCARGS2`.

Live toolu self-hosted and GHES lanes stay **unverified** unless a registered
runner is available.

## Review record

- Rev 1, adversarial review: Needs changes (0 blockers, 10 should-fix).
  Covered the task scan, the cooperative deadline, the `finish_execution`
  restructure, re-scan, the PID-reuse framing, the constructor list,
  degraded-scan reporting, AC-5 realism, cancel during cleanup, boundary
  cases, and doc rows.
- Rev 2, re-review: Needs changes (0 blockers, 3 should-fix: re-scan "new"
  semantics, AC label collision, AC-7 gate visibility; plus 5 considers).
- Rev 3 applies each rev-2 fix as worded by the reviewer. Approved.
