# Known limitations and resolved bugs

This page tracks current limitations and keeps the B-001–B-006 identifiers
used by tests and release history. All six tracked bugs are resolved; their
old reproduction steps do not describe the current implementation.

## Verification limits

The [compatibility matrix](compatibility-matrix.md) records acceptance status
per feature and lane; [test coverage](test-coverage.md) links the tests and
captures. A local replay does not prove GitHub.com or GHES behavior.

The manually dispatched [live workflow](../.github/workflows/live.yml) requires
the `TOOLU_RUNNER_LIVE_TOKEN` secret; its test repository is the
`TOOLU_RUNNER_LIVE_REPO` variable (default `Falconiere/toolu-runner-test`). The
[GHES workflow](../.github/workflows/live-ghes.yml) requires its corresponding
`TOOLU_RUNNER_LIVE_GHES_*` configuration. Missing credentials fail the lane as
`UNVERIFIED`; a test being present is not evidence that the live lane passed.

`live-ghes.yml` cannot prove GHES even with those credentials. The shared
harness reads only `TOOLU_RUNNER_LIVE_TOKEN` and `TOOLU_RUNNER_LIVE_REPO`
(`crates/toolu-runner/tests/helpers/live_harness.rs`, `require_live_env`) and
registers against github.com, so the `TOOLU_RUNNER_LIVE_GHES_*` values are never
consumed. GHES is out of scope: implemented, never run on a real server. No
GHES result exists to cite.

## Current limitations

- **Top-level `$/` steps (#241):** only `$/` inside an action manifest's
  composite steps is resolved. A workflow step `uses: $/…` is rewritten by the
  service before delivery; the delivered form (`repositoryType: self` without a
  `./` path) has not been observed, so it is intentionally not resolved until a
  live job shows it. The #251 paired probe contains no top-level `$/` step, so
  it did not observe that form either.
- **Outage reporting:** a missing job (renewal HTTP 404) cancels immediately;
  every other renewal failure counts toward the 300-second watchdog, including
  auth/protocol errors and malformed success replies. Completion still retries
  only network-class failures for up to ten minutes; an expired reporting token
  or a permanent 4xx can prevent delivery. A live six-minute network-outage
  test remains a separate, credentialed verification step. The first terminal
  renewal cause determines the annotation; a later 404 during teardown does not
  replace an already recorded timed outage.
- **Offline services:** `offline` mode starts the legacy v1 cache only. The
  artifact and OIDC service implementations are not started by the production
  job runner. Use `forwarder` for GitHub services or `accelerated` for local
  caching with upstream artifact/OIDC access.
- **S3 tier:** `[cache.l2]` is parsed and preserved, but production jobs do not
  construct an `L2Tier`. Enabling the section does not provide remote storage.
- **Host storage:** `_temp` and the file-command `tmp` directory are emptied at
  job start and end, as the official runner does, but `_tool` is never pruned:
  monitor its disk use on long-lived hosts and clean it by hand. A file the
  runner cannot delete, such as a root-owned file left by a container, stays
  with a `WARN` on each cleanup and never changes the job result. Registrations
  must not share a `data_dir`, because cleanup assumes one job at a time there.
  The job-end process sweep is not disk cleanup.
- **Job log storage:** with a Results Service endpoint, masked job and step logs
  are asynchronously spooled under `data_dir/_logs`, gzip-compressed on disk and
  streamed to the service; without one, spools are skipped. Step spools are
  independent, so a failed combined spool does not disable their uploads.
  Total job output no longer accumulates in memory, but requires space for plain
  and compressed files. Upload success, failure and task cancellation remove
  owned files; a process crash can leave spools behind. Starting the same job ID
  replaces its stale combined `.log` and `.log.gz`, not unrelated step files.
  A spool I/O failure warns and can leave durable logs incomplete without
  changing the job conclusion.
- **Job containers in accelerated mode:** a job with `container:` fails at
  "Set up job" under `[services] mode = "accelerated"` (or `offline`) with
  "job containers require forwarder services mode", because the local service
  URLs are loopback addresses the container cannot reach. Use `forwarder` for
  container jobs. Running the toolu runner itself inside a container is
  supported in accelerated mode; the #237 live probe runs it that way.
- **Secret-bearing environment URL (deliberate deviation):** when a job's
  `environment.url` evaluates to a value the masker would change, toolu sends
  no URL. The official runner (v2.338.2 `JobExtension.FinalizeJob`) logs the
  same "Skip setting environment url as environment '<name>' may contain
  secret." warning but keeps the original template; for a secret in a
  literal environment URL that template is the secret-bearing string, which it
  then reports. toolu suppresses both forms, so a registered secret never
  reaches `completejob` (`crates/execution/src/execution/environment_url.rs`).
  A URL built from a masked step output ends empty on both runners (#237).
- **Removal during a job:** normal removal drains a supervised registration
  for up to 30 seconds and keeps its state if still busy. `remove --force`
  removes local state while retaining the held lock; it neither cancels the
  job nor unregisters its GitHub runner. Remove that runner on GitHub afterward.

## Observed differences from the official runner (#251)

These come from one comparison: paired run
[42424784091](https://github.com/Falconiere/toolu-ghrunner/actions/runs/42424784091)
(head `0d23d0e`, official v2.338.2, Linux-X64, Linux-ARM64 and macOS-ARM64),
recorded in [`compat-reference-230.json`](compat-reference-230.json). In it all 110
paired jobs had equal conclusions and ordered step lists, with no acknowledged
difference, so what follows is what that comparison did not make equal, not a
compatibility guarantee beyond the jobs, platforms and versions it covers. See
[compatibility-evidence.md](compatibility-evidence.md#paired-run-42424784091-251).

- **First lock renewal (accepted as non-material here).** Upstream's
  `JobDispatcher.RenewJobRequestAsync` renews immediately after acquire, waits for
  that first renewal before starting the worker, then renews every 60 s. toolu's
  `listener::helpers::spawn_renewal` skips the first immediate tick of a 60 s
  `tokio` interval, so it first renews at 60 s and then every 60 s. On the 150 s
  `long` job each platform recorded 2 renewals on toolu and 3 on the official
  runner; both meet the R-renew floor of at least 2, and job results, outputs and
  stored data were equal. No runtime change was made.
- **Error text after a failed "Initialize containers".** In the paired `negative`
  `stack` job (a job image no registry serves) both runners report identical rows:
  "Initialize containers" fails, every `success()` step is skipped, the `always()`
  step "Return workspace ownership" fails, then "Stop containers" succeeds and
  "Complete job" follows, with equal conclusions. Only the log text differs. The
  official log carries `Docker pull failed with exit code 1` then
  `Value cannot be null. (Parameter 'ContainerId')`; toolu logs the Docker
  daemon's pull error then `docker error: the job container is not running`
  (`steps_runner.rs`, `post_drain.rs`). toolu never runs such a step on the host.
- **Official-runner stderr/mask race (official lane, not toolu).** In the step that
  registers a mask with `::add-mask::`, the official runner can log a stderr line
  before it has processed the stdout command (run 42407955591; the runner reads
  the two streams separately). The `leaks` probe therefore echoes the value to
  stderr only in the next step, so the leak count compares like with like. toolu
  showed no leak in run 42424784091 (0 hits on every lane).
- **Fixed since earlier runs (no remaining difference observed).** The message-poll
  HTTP 404 WARNs seen on toolu lanes in runs 42389172867 and 42403405937 came from
  polling `status=Online` while a job ran; upstream's `BrokerMessageListener` polls
  with the dispatcher's status, `Busy`, which toolu now sends (D2). Idle long polls
  also failed about every 61 s because toolu's 60 s client timeout equalled the
  broker's idle hold; the broker client timeout is now 100 s, upstream's default
  (D4). Run 42424784091 recorded `poll_failures` 0 on every lane of every platform.

## Resolved bugs

| ID | Original problem | Current behavior and regression coverage |
| --- | --- | --- |
| B-001 | No prolonged-outage watchdog | `listener::outage::OutageWatchdog` cancels after more than 300 seconds without a successful renewal on any renewal error, observed on the 60-second cadence; renewal 404 cancels immediately. A non-success result becomes failure, with a job-lock-loss annotation for 404 or a lost-connection annotation for a timed outage; success during teardown stays success. Covered by `crates/listener/src/tests/watchdog_trip.rs`, `watchdog_retry.rs` and `watchdog_missing_job.rs`. |
| B-002 | Local removal left a GitHub registration behind | `wire::net::register::unregister_runner` unregisters repo/org runners before local deletion, confirming absence after a 404. Failure preserves retryable state. Covered by `crates/toolu-runner/tests/remove_unregister_test.rs`. |
| B-003 | Registration wrote placeholder credentials | `register_jit` calls GitHub's `generate-jitconfig` endpoint and persists the returned JIT blob and runner ID. Covered by `crates/toolu-runner/tests/gh_compat_register.rs`; live end-to-end verification is a separate lane. |
| B-004 | Shell, Node and composite temp/tool-cache paths diverged | Shared `runner_temp_dir` / `runner_tool_cache_dir` helpers supply `_temp` / `_tool`. Covered by `crates/execution/tests/runner_paths_test.rs` and `composite_runner_temp_test.rs`. |
| B-005 | Composite expressions lost populated `runner.*` fields | Composites use the full expression engine over the invocation's context snapshot. Covered by `crates/execution/tests/composite_runner_context_test.rs`. |
| B-006 | Forwarder cache fell back to retired v1 | `execution::service_endpoints` reads `actions_uses_cache_service_v2` (case-insensitive name, on only for boolean true) and `actions_cache_mode`, and hands each step type its official `ACTIONS_*` set: Node gets the v2 flag (`True`) and mode, container actions the same without the flag, `run:` steps only the OIDC pair. The setup log prints `Cache mode: <mode>`. Covered by `crates/toolu-runner/tests/gh_compat_forwarder.rs` and `crates/listener/src/tests/setup_step.rs`. |
