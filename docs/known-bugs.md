# Known Bugs

Tracking format: B-NNN — short title — severity — owner — status.

## Live smoke deferred (waiting on user)

- The runner's full end-to-end live path (register → run → execute →
  report) is blocked on step 10, which requires a real registration
  token from a test repo. Until the user provides one, the entries
  below are tracked.

## B-001 — Outage > 5 min mid-job: cancellation watchdog missing

- **Severity:** Medium
- **Owner:** TBD (likely listener maintainer)
- **Status:** Resolved
- **Resolution:** A pure `OutageWatchdog` (`listener/src/outage.rs`,
  300 s threshold, write-once latch) rides the existing 60 s lock-renewal
  heartbeat in `helpers::spawn_renewal`. `wire::net::run_service`
  classifies `renew_job` failures so only `RunnerError::Network`-class
  errors (transport failure, 429, 5xx — GitHub is unreachable) feed the
  watchdog; a definitive `Protocol`/`Auth` renewal error only WARNs and
  never trips it, so a persistent 401/404 can't manufacture a false "lost
  connection". On trip the watchdog sets the shared flag once, cancels the
  job's `CancellationToken` (the engine SIGKILLs the running step), and
  logs one ERROR; detection lands 5–6 minutes after the last successful
  renewal. `execution_loop::execute_with_renewal` joins the renewal task
  before reading the flag and, when tripped with a non-`Success`
  conclusion, overrides the report to `Failure` with the annotation
  "Runner lost connection to GitHub for more than 5 minutes; job was
  cancelled (lost connection)." Report-on-reconnect: `poll_and_execute`
  demotes `acknowledge_message` to a single-attempt, WARN-only,
  non-gating call, and wraps `complete_job` in
  `listener/src/retry.rs::retry_transient` — jittered 1s → 60s backoff,
  cancel-aware, retrying `Network` errors only, up to a 10-minute total
  budget — so the completion report survives a still-recovering
  connection instead of being lost when the always-online loop re-polls.
  Two residual risks are accepted and unchanged by this fix: (1) the
  OAuth bearer / `rs_token` are minted once per listener lifecycle and
  assumed to outlive the outage — if one expires mid-outage,
  `complete_job` gets a definitive 4xx that stops the retry and the
  report is lost, with GitHub's server-side job timeout as the backstop;
  (2) detection covers `Network`-class renewal failures only — a
  persistent *definitive* renewal error still leaves the job running with
  a dead reporting channel, exactly as before this fix. Live `tc` netem
  outage e2e remains deferred to the step-10 live smoke (see Reproduce,
  below).
- **Trigger:** The runner's network drops for more than 5 minutes
  during a job. The in-flight step keeps running locally, but the
  reporting channel is offline. The spec requires the runner to
  cancel the job with reason "lost connection" and report `failure`
  to GH.
- **Observed:** The runner has no watchdog that detects prolonged
  outages. The job blocks indefinitely.
- **Expected:** The listener tracks connection uptime; on > 5 min of
  failed reporting, it cancels the in-flight `CancellationToken`,
  waits for the step to terminate, and reports the job as `failure`
  to GH on reconnect.
- **Reproduce:** `cargo test --features e2e-live` against a test repo
  with `tc` simulating 6+ min of network outage mid-job.

## B-002 — Live unregistration API call not implemented

- **Severity:** Low (deferred to step 10)
- **Owner:** step 10
- **Status:** Resolved
- **Resolution:** `remove` now unregisters on GitHub before deleting
  anything local. `wire::net::register::unregister_runner` DELETEs
  `…/actions/runners/{id}` using the persisted `runner_id`, falling back to a
  name lookup when no id was recorded. A 404 is NOT taken as success on its
  own — GitHub answers 404 for resources a token may not see, so it is
  confirmed by a lookup the token must be able to perform; only a lookup that
  succeeds and finds nothing proves absence. Without that, an under-scoped
  token would be told "unregistered" right before the only local handle on a
  still-registered runner was deleted: B-002 again, dressed as success.
  Ordering is deliberate — the persisted `runner_id` and URL are the only way
  to name the runner, so a failed unregister aborts with local state intact
  and the removal can be retried.

  Whether a run is in flight is decided by **acquiring** the job lock
  (`config::lockfile::acquire`), not by inspecting the file: that is atomic,
  reuses this crate's stale-lock rule, and holding the guard across the
  multi-second DELETE closes the window where a `run` could start mid-removal.
  A leftover `.lock` nobody holds — the resting state of any machine that has
  run a job, since nothing deletes it on a normal exit — is therefore not
  "in flight", so the ordinary `remove` still reaches the unregister. When the
  lock IS held, `--force` removes local state but keeps `.lock` (fs2's lock is
  inode-scoped, so unlinking it would let a second `run` acquire a fresh one
  and race the live job) and skips the unregister, since that job still
  reports against this registration.

  Bearer precedence is `--token` > `TOOLU_RUNNER_TOKEN` > the stored `login`
  token, with an empty value counting as absent. No token at all is an
  **error**, not a warning-and-continue: deleting the persisted id and URL
  while the runner is still registered is the very outcome this bug is about,
  and a WARN scrolls past. `--skip-unregister` is the explicit opt-out, and a
  `RunnerError::Config` (an org-level URL the repo-scoped API cannot address)
  degrades to that same skip since no retry could ever resolve it. Live
  cancellation of a running job remains step 10 work.
- **Trigger:** `toolu-runner remove` is called while a registration
  exists.
- **Observed:** The CLI writes `.pending_remove` if a `run` is in
  flight (and refuses), or with `--force` cancels the in-flight
  job. With no `run` active, `remove` deletes `config.toml` and
  `credentials.json` locally. Neither path calls the GH
  unregistration endpoint.
- **Expected:** `remove` sends a DELETE to
  `https://api.github.com/repos/{owner}/{repo}/actions/runners/{runner_id}`
  (or the GHES equivalent), waits for 204, then deletes the local
  `config.toml` and `credentials.json`.

## B-003 — Live register POST to JIT endpoint not exercised

- **Severity:** Low
- **Owner:** E0–E3 (gh-compatibility-core)
- **Status:** Resolved
- **Resolution:** Live JIT register implemented in `net/register.rs`
  (POST `…/repos/{owner}/{repo}/actions/runners/generate-jitconfig` →
  persists the real `encoded_jit_config` + `runner_id`), replacing the
  placeholder stub. End-to-end live smoke (real-token register → run)
  is still pending a registration token from a test repo (tracked by
  S16).
- **Trigger:** `toolu-runner register` is called.
- **Observed:** Step 9 wrote the URL validation and JIT endpoint
  derivation (`jit_endpoint_for_host` returns
  `https://pipelinesgh.azureedge.net` for `github.com`,
  `https://pipelines.<host>` for GHES), and the CLI probes that
  endpoint with a 5s HEAD before accepting the registration. But
  the actual POST to the JIT endpoint with the registration token
  to get the JIT config blob, and the subsequent JWT exchange for
  an OAuth2 token, are stubbed — the CLI writes a placeholder
  `auth_token` and an empty `jit_config`.
- **Expected:** The live smoke in step 10 will exercise the
  end-to-end registration flow:
  `POST <jit_endpoint>` with the registration token →
  parse the JIT config → RSA key reconstruction →
  PS256 JWT → OAuth2 exchange → write the real `auth_token`
  and base64 `jit_config` to `~/.toolu-runner/config.toml`.