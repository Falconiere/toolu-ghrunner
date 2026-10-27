# Known limitations and resolved bugs

This page tracks current limitations and keeps the B-001–B-005 identifiers
used by tests and release history. All five original bugs are resolved; their
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

## Current limitations

- **Forwarder cache:** the job-variable lookup does not recognize GitHub's
  `actions_uses_cache_service_v2`, and `ACTIONS_CACHE_MODE` is not exported.
  The [paired run](compatibility-evidence.md#recorded-differences) recorded
  a fallback to retired v1 endpoints: saves warned without creating an entry
  and restores failed. A successful cache-save step alone does not prove a hit.
- **Docker reporting:** `docker://` actions do not emit the official runner's
  separate `Pull <image>` setup step. The same paired run found matching shared
  step conclusions and container outputs, but not identical setup reporting.
- **Outage reporting:** the watchdog counts only network-class renewal
  failures (transport errors, 429 and 5xx). Definitive auth/protocol failures
  warn without tripping it. Completion retries network failures for up to ten
  minutes; an expired reporting token or a permanent 4xx can still prevent
  delivery. A live six-minute network-outage test remains a separate,
  credentialed verification step.
- **Offline services:** `offline` mode starts the legacy v1 cache only. The
  artifact and OIDC service implementations are not started by the production
  job runner. Use `forwarder` for GitHub services or `accelerated` for local
  caching with upstream artifact/OIDC access.
- **S3 tier:** `[cache.l2]` is parsed and preserved, but production jobs do not
  construct an `L2Tier`. Enabling the section does not provide remote storage.
- **Host storage:** workspace GC does not prune the registration's `_temp`,
  `_tool` or file-command `tmp` directories. Monitor disk use on long-lived
  hosts; the job-end process sweep is not disk cleanup.
- **Job log memory:** the listener keeps every masked log line of a job in
  memory (`execution_loop` `all_job_lines`) for the combined log upload, so a
  long, verbose job grows the runner's memory until it completes.
- **Removal during a job:** normal removal drains a supervised registration
  for up to 30 seconds and keeps its state if still busy. `remove --force`
  removes local state while retaining the held lock; it neither cancels the
  job nor unregisters its GitHub runner. Remove that runner on GitHub afterward.

## Resolved bugs

| ID | Original problem | Current behavior and regression coverage |
| --- | --- | --- |
| B-001 | No prolonged-outage watchdog | `listener::outage::OutageWatchdog` cancels after 300 seconds of network-class renewal failures, observed on the 60-second renewal cadence. A non-success result becomes failure with a lost-connection annotation; success during teardown stays success. Covered by `crates/listener/src/tests/watchdog_trip.rs` and `watchdog_retry.rs`. |
| B-002 | Local removal left a GitHub registration behind | `wire::net::register::unregister_runner` unregisters repo/org runners before local deletion, confirming absence after a 404. Failure preserves retryable state. Covered by `crates/toolu-runner/tests/remove_unregister_test.rs`. |
| B-003 | Registration wrote placeholder credentials | `register_jit` calls GitHub's `generate-jitconfig` endpoint and persists the returned JIT blob and runner ID. Covered by `crates/toolu-runner/tests/gh_compat_register.rs`; live end-to-end verification is a separate lane. |
| B-004 | Shell, Node and composite temp/tool-cache paths diverged | Shared `runner_temp_dir` / `runner_tool_cache_dir` helpers supply `_temp` / `_tool`. Covered by `crates/execution/tests/runner_paths_test.rs` and `composite_runner_temp_test.rs`. |
| B-005 | Composite expressions lost populated `runner.*` fields | Composites use the full expression engine over the invocation's context snapshot. Covered by `crates/execution/tests/composite_runner_context_test.rs`. |
