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
  live job shows it. Live check: #251.
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
- **Removal during a job:** normal removal drains a supervised registration
  for up to 30 seconds and keeps its state if still busy. `remove --force`
  removes local state while retaining the held lock; it neither cancels the
  job nor unregisters its GitHub runner. Remove that runner on GitHub afterward.

## Resolved bugs

| ID | Original problem | Current behavior and regression coverage |
| --- | --- | --- |
| B-001 | No prolonged-outage watchdog | `listener::outage::OutageWatchdog` cancels after more than 300 seconds without a successful renewal on any renewal error, observed on the 60-second cadence; renewal 404 cancels immediately. A non-success result becomes failure, with a job-lock-loss annotation for 404 or a lost-connection annotation for a timed outage; success during teardown stays success. Covered by `crates/listener/src/tests/watchdog_trip.rs`, `watchdog_retry.rs` and `watchdog_missing_job.rs`. |
| B-002 | Local removal left a GitHub registration behind | `wire::net::register::unregister_runner` unregisters repo/org runners before local deletion, confirming absence after a 404. Failure preserves retryable state. Covered by `crates/toolu-runner/tests/remove_unregister_test.rs`. |
| B-003 | Registration wrote placeholder credentials | `register_jit` calls GitHub's `generate-jitconfig` endpoint and persists the returned JIT blob and runner ID. Covered by `crates/toolu-runner/tests/gh_compat_register.rs`; live end-to-end verification is a separate lane. |
| B-004 | Shell, Node and composite temp/tool-cache paths diverged | Shared `runner_temp_dir` / `runner_tool_cache_dir` helpers supply `_temp` / `_tool`. Covered by `crates/execution/tests/runner_paths_test.rs` and `composite_runner_temp_test.rs`. |
| B-005 | Composite expressions lost populated `runner.*` fields | Composites use the full expression engine over the invocation's context snapshot. Covered by `crates/execution/tests/composite_runner_context_test.rs`. |
| B-006 | Forwarder cache fell back to retired v1 | `execution::service_endpoints` reads `actions_uses_cache_service_v2` (case-insensitive name, on only for boolean true) and `actions_cache_mode`, and hands each step type its official `ACTIONS_*` set: Node gets the v2 flag (`True`) and mode, container actions the same without the flag, `run:` steps only the OIDC pair. The setup log prints `Cache mode: <mode>`. Covered by `crates/toolu-runner/tests/gh_compat_forwarder.rs` and `crates/listener/src/tests/setup_step.rs`. |
