# Known bugs — toolu-runner

Populated per AC #21 from `docs/toolu/specs/2026-06-18-toolu-runner-standalone-design.md`.

Each entry: **trigger** · **observed** · **expected** · **severity** · **status**.

## Live smoke

- **deferred** — owner: user, blocked on registration token for a test
  repo. First live pass not yet run as of 2026-06-18. Until the live pass
  is complete, the "Known bugs" list below is the pre-live set derived
  from spec scenarios that require real GH responses or a sustained
  failure (mid-job network outage, disk full under load).

## Known bugs

### B-001 — Outage > 5 min mid-job not yet enforced
- **Trigger:** `run` is mid-job when the network drops and stays down for
  longer than 5 minutes.
- **Observed:** The polling loop retries with exponential backoff capped
  at 60s; the in-flight step continues running locally. The runner never
  cancels the job with "lost connection" — the renewal task will simply
  keep failing.
- **Expected:** After 5 minutes of unrecoverable network failure, the
  job is cancelled with reason `lost connection` and reported as
  `failure` to GH.
- **Severity:** Medium. The 5-min cancellation is in the spec but
  requires a separate watchdog timer that owns the `cancel` token; the
  renewal task already warns on each failure, so the user sees the
  condition, just without the spec-mandated `failure` outcome.
- **Status:** Tracked. Implementation lands after the live smoke surfaces
  it as a real problem or after the renewal task is refactored to own
  the cancellation. Currently the failure mode is observable but not
  acted on.

### B-002 — `remove` mid-job live unregistration not yet implemented
- **Trigger:** `toolu-runner remove` invoked while a `run` is mid-job.
- **Observed:** The runner writes `.pending_remove` and refuses with
  exit 2, OR (with `--force`) it cancels the listener's `cancel` token
  and deletes `config.toml` + `credentials.json` locally. Neither path
  calls the actual GH unregistration endpoint.
- **Expected:** The full unregistration call to
  `POST {api}/actions/runner-registration` with the unregister token,
  followed by file deletion. With `--force`, the in-flight job is
  cancelled first, then unregistration is called, then files are
  deleted.
- **Severity:** Low. The data on disk stays consistent (a re-`register`
  cleans up); the GH side eventually expires the JIT session server-side
  (typically within minutes).
- **Status:** Live GH call lands in step 10 alongside the full
  `register` flow.

### B-003 — `register` does not yet POST to the JIT endpoint
- **Trigger:** `toolu-runner register --url <repo> --token <rt>`.
- **Observed:** The CLI validates the URL (rejects non-`github.com` /
  non-GHES hosts, rejects invalid URLs) and writes a placeholder JSON
  config + creds file. No HTTP call to the JIT endpoint yet.
- **Expected:** POST to
  `https://pipelinesgh.azureedge.net/.../runnerregistration` with the
  registration token, parse the JIT config from the response, exchange
  the embedded JWT for an OAuth2 token, and write the real `auth_token`
  + base64 `jit_config` to disk.
- **Severity:** High (blocks live use) but expected at this point in
  the port.
- **Status:** Lands in step 10 (live smoke).

### B-004 — GHES V1 path covered by unit tests, not live
- **Trigger:** `register --url https://ghes.example.com/...`.
- **Observed:** URL validation accepts the host; protocol_version is
  set to `v1`. No actual JIT probe yet (lands in step 10).
- **Expected:** A real registration + run on a live GHES instance.
- **Severity:** Medium.
- **Status:** Blocked on a test GHES instance. Spec marks this as
  "ship without GHES live-tested in v1, mark as known bug."

### B-005 — Disk-full mid-job uses the generic IO error path
- **Trigger:** `run` is mid-job and the workspace fills up.
- **Observed:** The current step fails with `RunnerError::Io` (kind
  `StorageFull` / `ENOSPC`). The step is reported as `failure` and the
  job completes with `failure`. The `run` process stays alive for the
  next job (the listener does not exit on job failure).
- **Expected:** Same. The spec says "logs the error, marks the current
  step `failure`, completes the job `failure`" — the existing engine
  flow does this, just via the generic IO error variant rather than a
  dedicated `DiskFull` arm.
- **Severity:** None — behavior matches the spec.
- **Status:** No action. Listed here so reviewers don't think it was
  forgotten.

### B-006 — Stale-lock recovery uses the holder PID's liveness as the
            primary signal
- **Trigger:** A `run` process crashes (SIGKILL, OOM, power loss) while
  holding `.lock`.
- **Observed:** The next `run` reads the body, checks `is_pid_alive`,
  and if the PID is dead (which it is for any killed process) AND the
  mtime is older than 5 minutes, removes the lock and acquires it.
- **Expected:** Same.
- **Severity:** Low.
- **Status:** No action.
- **Note:** A second `run` that arrives within 5 minutes of the crash
  will *not* steal the lock; the spec says "the next `run` that
  observes a stale mtime" — so 5 minutes is the floor. Acceptable.
