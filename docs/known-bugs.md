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

## B-004 — `RUNNER_TOOL_CACHE` / `RUNNER_TEMP` diverge between step kinds

- **Severity:** Medium
- **Owner:** TBD (execution maintainer)
- **Status:** Resolved
- **Resolution:** All writers now share one source:
  `context::runner_temp_dir` / `runner_tool_cache_dir` (pub, beside
  `set_runner_context`, which itself uses them) return `data_dir/_temp`
  / `data_dir/_tool`; `action_support::apply_runner_paths` (node stages
  at any nesting depth) and `composite_env::build_step_env` read them
  back instead of joining their own `tmp` / `tool_cache` paths, and
  `composite_exec` threads a dedicated `runner_temp` (distinct from the
  composite's file-command backing dir, which deliberately stays
  `data_dir/tmp`) into both composite interpolation sites — so
  `${{ runner.temp }}` in a composite `run:` body or step `env:` now
  equals the step's own `$RUNNER_TEMP`. Ordering: `set_runner_context`
  runs during job context build (`job_runner.rs:601`), before any step,
  creating both dirs 0700; a failed creation WARNs and degrades to the
  old missing-dir behavior (`@actions/tool-cache` self-heals with
  `mkdirP`), never worse than before. The units live in private
  modules, so coverage drives public surfaces:
  `crates/execution/tests/composite_runner_temp_test.rs` (a real bash
  subprocess through `execute_composite_action`, pinning env ==
  script-body interpolation == `env:`-value interpolation ==
  `data_dir/_temp`), `crates/execution/tests/runner_paths_test.rs`
  (pins the `_temp` / `_tool` literals; the wiring is structural via
  the shared helpers), and the pre-existing `gh_compat_context.rs`
  `${{ runner.tool_cache }}` pin. Existing `data_dir/tool_cache` trees
  on live runners are orphaned — one cold re-download, accepted. Still
  open by design, tracked here: the file-command backing dir remains
  `data_dir/tmp` (internal, never env-exposed) and none of `_temp` /
  `_tool` / `tmp` is GC'd.
- **Trigger:** Any job that addresses `$RUNNER_TOOL_CACHE` /
  `$RUNNER_TEMP` / `${{ runner.tool_cache }}` across the boundary
  between `run:` steps and node-action stages (or composite inline
  `run:` steps).
- **Observed:** The job-level env (`context.rs::set_runner_context`,
  `crates/execution/src/execution/context.rs:121-126,141-142`) sets
  `RUNNER_TEMP=data_dir/_temp` and `RUNNER_TOOL_CACHE=data_dir/_tool`
  (both created, 0700) — what top-level `run:` steps and the real
  `${{ }}` evaluator see (pinned by
  `crates/toolu-runner/tests/gh_compat_context.rs:90-91`). But every
  node-action stage (`action_support.rs::apply_runner_paths`,
  `crates/execution/src/execution/action_support.rs:66-83`, via
  `build_node_env`) **overwrites** them with `data_dir/tmp` /
  `data_dir/tool_cache` — and `tool_cache` is created by nothing and
  pinned by no test. Composite inline `run:` steps get the wrong
  `tmp` temp too (`composite_env.rs:84,120-122`; their tool_cache
  correctly inherits `_tool`), and nested `uses:` steps re-enter
  `action_exec::execute_action` (`composite_uses.rs:108-114`) so
  inner node stages diverge identically at any depth. The two value
  sets were born two days apart in different scaffolding passes
  (`action_support.rs` in `3814f45`; `set_runner_context` in
  `17eaa11`, resolving "Open Q6" only in `context.rs`) and have
  never agreed at any commit. Adjacent inconsistencies: a top-level
  `run:` step's own `$RUNNER_TEMP` (`_temp`) differs from the dir
  backing its `$GITHUB_ENV`/`$GITHUB_OUTPUT` file commands (`tmp`,
  `steps_runner.rs:413`), and none of `_temp` / `_tool` / `tmp` /
  `tool_cache` is ever GC'd (`workspace_gc` sweeps `workspace_root`
  children only) — unbounded growth on long-lived host-mode
  registrations.
  Ranked impact: (1) caching `${{ runner.tool_cache }}` with
  `actions/cache` to persist `setup-*` installs saves/restores the
  empty `_tool` tree forever — a silent, permanent cache-miss loop,
  since `setup-node`/`-go`/`-java`/`-python` (node actions) really
  install into `tool_cache`; (2) a `run:` step installing into
  `$RUNNER_TOOL_CACHE` is invisible to a later node action's
  tool-cache `find()` (miss → re-download; breaks airgapped setups);
  (3) `run:` steps enumerating `$RUNNER_TOOL_CACHE` after a
  `setup-*` action see an empty dir (PATH-based "install then invoke
  by name" is unaffected — `core.addPath` works since `aa8a5ca`);
  (4) two cache trees accumulate on disk.
- **Expected:** One value per variable for the whole job, as in
  upstream `actions/runner` (underscore-prefixed `_tool` / `_temp`).
  The fix converges `action_support.rs::apply_runner_paths` and
  `composite_env.rs` on the `context.rs` values — not the reverse:
  `_tool`/`_temp` are the majority value today, the only side that
  is disk-real (pre-created 0700) and test-pinned, and the upstream
  naming. Existing `data_dir/tool_cache` trees on live runners are
  orphaned by the change (acceptable: one cold re-download).
- **Reproduce:** Job with `actions/setup-node@v4` followed by
  `run: ls -la "$RUNNER_TOOL_CACHE"` — the run step lists an empty
  `data_dir/_tool` while the node install landed in
  `data_dir/tool_cache`.

## B-005 — Composite interpolator resolves real `runner.*` fields to `""`

- **Severity:** Medium
- **Owner:** TBD (execution maintainer)
- **Status:** Resolved
- **Resolution:** `resolve_runner` no longer hand-implements a second,
  independently-maintained copy of the `runner.*` values — it now takes
  `ctx: &ExecutionContext` (replacing the bare `temp_dir: &Path` it used to
  thread through) and answers every key via a new
  `ExecutionContext::runner_value(key) -> Option<&str>` accessor
  (`context.rs`, mirroring the existing `github_context` accessor), which
  reads the SAME `runner_context` map `set_runner_context` populates
  (`os`/`arch`/`name`/`temp`/`tool_cache`, plus `debug` when step-debug is
  on) and `eval_context()` hands to the real evaluator. Sourcing this way
  means every field composite interpolation answers is structurally the
  same value the real evaluator answers — there is no second place a future
  field could be added to one and not the other. An absent key (a
  genuinely unknown field, or `debug` when step-debug is off) still
  resolves to `""`, unchanged upstream semantics. Threading: all three
  interpolation call sites (`composite_exec::run_run_step`,
  `composite_env::build_step_env`, `composite_uses::build_nested_step` /
  `build_inputs_token`) already had an `ExecutionContext` reference in
  scope, so no new struct was needed — the dedicated `runner_temp: &Path`
  field B-004 had threaded through `CompositeRun` and
  `NestedUsesParams` purely to reach these interpolation call sites became
  redundant and was removed (the `env:`-var-setting uses of
  `runner_temp_dir`/`runner_tool_cache_dir` from B-004 are untouched).
  `crates/execution/tests/composite_runner_context_test.rs` pins
  `${{ runner.tool_cache }}` at both the `run:` body and step `env:` sites
  to `data_dir/_tool`, pins `os`/`arch`/`name` to the same source
  `set_runner_context` uses, and pins a genuinely unknown field
  (`runner.bogus`) to `""`; `composite_runner_temp_test.rs` (B-004) now
  also calls `set_runner_context` before running its probe composite,
  mirroring real job-start sequencing now that interpolation reads
  through it rather than an independently-computed path.
- **Trigger:** A composite action using `${{ runner.tool_cache }}`
  (or any `runner.*` field other than `os` / `arch` / `temp`) in an
  inline `run:` body, a step `env:` value, or a nested `uses:`
  `with:` value.
- **Observed:** Composite steps do not route these fields through the
  real `expressions` evaluator — the hand-rolled
  `composite_expr.rs::resolve_runner`
  (`crates/execution/src/execution/composite_expr.rs:81-88`)
  implements only `os`, `arch`, and `temp`, and maps every other
  field to `String::new()`. `"${{ runner.tool_cache }}/x"`
  interpolates to `"/x"` — a root-relative path, which the boot
  container (running as root) will happily create at `/`. The real
  evaluator answers `data_dir/_tool` for the same expression one
  stack frame up (`gh_compat_context.rs:91`), so the same YAML line
  changes meaning depending on whether it appears in a workflow file
  or inside a composite action. (Upstream expression semantics do
  yield `""` for genuinely unknown properties; the bug is that
  documented, populated fields — `tool_cache` foremost — are treated
  as unknown here.)
- **Expected:** Composite interpolation answers the documented
  `runner.*` fields identically to the real evaluator — either by
  adding the missing arms sourced from the same values
  `set_runner_context` establishes, or by routing composite
  interpolation through the `expressions` crate. Fixing B-004 first
  settles which path `tool_cache` must report.
- **Reproduce:** Composite action step
  `run: echo "[${{ runner.tool_cache }}]"` prints `[]`; the same
  line in a workflow `run:` step prints `[<data_dir>/_tool]`.