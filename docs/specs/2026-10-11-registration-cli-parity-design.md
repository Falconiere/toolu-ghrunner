# Registration CLI parity: `--ephemeral` and `--no-default-labels` — Design

**Date:** 2026-10-11   **Status:** Approved   **Author:** Claude (epic #67 worker)   **Topic:** issue #92 — upstream `config.sh --ephemeral` / `--no-default-labels` on `toolu-runner register`

## Problem

`register` cannot opt out of its default labels, and `--labels` *replaces*
them instead of adding to them as the official runner does. The defaults use
Rust's spelling (`linux`, `x86_64`, `aarch64`), so the documented
`runs-on: [self-hosted, linux, x64]` never matches a toolu runner. There is no
ephemeral mode: `run --once` exits after a job but leaves the spent
registration behind and is not recorded anywhere, so a supervisor or
autoscaler cannot hand toolu a "one job, then clean up" registration.

Upstream reference (actions/runner `cab9d1c`): `CommandSettings.cs`
(`--ephemeral`, `--no-default-labels`, `GetLabels`), `Validators.cs`
(`LabelsValidator`), `ConfigurationManager.cs` (`CreateNewAgent`,
`DeleteLocalRunnerConfig`), `Runner.cs` (`settings.Ephemeral` → run once →
`DeleteLocalRunnerConfig` after the job).

## Non-Goals

1. `--disableupdate`, `--pat`, `--local` and other upstream config flags.
2. A generic "the runner was deleted on GitHub → delete local config" path for
   non-ephemeral runs (upstream's `cleanupLocalConfigAfter404`). Separate issue.
3. Any change to `boot` (already server-ephemeral, no persisted state) or to
   `run --once` on a non-ephemeral registration.
4. Org-level unregister (the repo-scoped runners API cannot address org URLs —
   same limit `remove` already documents).
5. Re-labelling existing registrations: re-mint keeps the persisted labels.

## Architecture

**Labels.** A new pure `config::labels` module owns label resolution, so the
CLI, tests and future callers share one rule set, mirroring upstream:

- Defaults are `self-hosted`, `shared::platform::runner_os()`
  (`Linux`/`macOS`), `shared::platform::runner_arch()` (`X64`/`ARM64`) — the
  same spelling GitHub uses for `RUNNER_OS`/`RUNNER_ARCH`.
- `--labels` values are *additional*. Empty entries are dropped (`a,,b`).
  Labels longer than 256 characters are rejected. The combined list
  (defaults first, then user labels in order) is de-duplicated
  case-insensitively, keeping the first spelling.
- `--no-default-labels` omits the defaults; with no remaining user label it
  fails with upstream's message before any network call.

The live probe (below) shows GitHub stores exactly the labels sent, rejects an
empty list with 422, and itself de-duplicates case-insensitively — so the
client-side rules make the persisted `labels` equal to GitHub's set.

**Ephemeral registration.** `register --ephemeral` persists a top-level
`ephemeral = true` key in `config.toml` (`#[serde(default)]`, so older configs
load as `false`). Every toolu registration is a JIT runner, which GitHub
already treats as single-job; the new flag adds the *client* lifecycle that
upstream's `--ephemeral` implies:

- `run` on an ephemeral registration never re-mints. A pure
  `listener::loop_decision::next_ephemeral_action` decides each iteration
  (first match wins):
  1. a job ran (`Ok(Some(conclusion))`, any conclusion, even after a cancel)
     → `Unregister`;
  2. cancelled → `Exit(0)`, registration kept;
  3. `--once` → `Exit(0)` on `Ok(None)`, `Exit(5)` on a session conflict,
     `Exit(1)` on any other error (same as today's `--once`);
  4. `.pending_remove` → `Exit(0)`, registration kept;
  5. `Err(SessionConflict)` → `Exit(5)`;
  6. `Err(Auth)` → `Exit(1)`, registration kept (the JIT is dead and an
     ephemeral runner never re-mints);
  7. `Ok(None)` or any other `Err` → `BackoffRetry` on the same, still unused
     JIT config.
- `Unregister` (new `toolu-runner/src/ephemeral.rs`): resolve a bearer
  (`TOOLU_RUNNER_TOKEN` > stored `login`, via the existing `remove_bearer`).
  With one, call the existing `wire::net::unregister_runner` (DELETE; a 404 is
  confirmed absent by a name lookup). Success → "unregistered on GitHub".
  A `RunnerError::Config` (org URL the API cannot address) or no bearer → the
  local state still goes, and the message says GitHub removes a used JIT
  runner itself and that this was **not verified**. Any other error → local
  state is **kept**, `run` exits 2 naming `toolu-runner remove` as the retry.
  After a successful or unverified unregister, `config.toml`,
  `credentials.json`, `.pending_remove` and `.lock` are deleted via the
  existing `delete_registration_state` (`_diag/` kept), and `run` exits 0.
- `install-service` refuses an ephemeral registration (except `--remove`):
  a restart-forever unit would relaunch into a removed registration.
- `run` does not warn about a missing login token for an ephemeral
  registration (no re-mint will happen).

Decisive trade-off: verify removal with an API call when a token exists,
instead of trusting server-side removal as upstream does. It costs one request
per ephemeral job and lets toolu say truthfully whether cleanup happened.

Reused paths: `remove_bearer`, `delete_registration_state`,
`UNREGISTER_TIMEOUT` (crate root), `wire::net::unregister_runner`,
`shared::platform`, `run_cmd::RunLoop` backoff.

**Deleted registration (found in the live lane).** GitHub answers the OAuth
token exchange of a JIT registration whose runner was deleted with **400**
`{"error":"invalid_client","error_description":"Registration … was not
found."}` (live capture `crates/wire/tests/oauth_invalid_client_400.json`).
toolu mapped any 400 to `RunnerError::Protocol`, so an ephemeral run — and
the default loop — retried the dead registration forever (150 s observed).
Upstream (`MessageListener.cs`) treats `invalid_client` as "registration
deleted" and stops. `wire::net::auth::exchange_token` now returns
`RunnerError::Auth` for a 4xx reply whose JSON `error` is `invalid_client`;
other replies keep their classification (a 5xx stays `Network`). Effect: the
ephemeral rule 6 (`Auth` → exit 1, state kept) applies, and the default loop
re-mints (its existing rule for a dead JIT) instead of retrying forever.

**Mode contract (92-S4).**

| Mode | Server registration | Jobs per process | After the job |
| --- | --- | --- | --- |
| `run` (default) | JIT, re-minted per job | unlimited | re-mint, keep polling |
| `run --once` | JIT | 1 | exit; spent local state kept |
| `run` on `--ephemeral` | JIT | 1 | unregister (verified if token) + delete local state |
| `boot` | JIT from env, nothing persisted | 1 | exit; GitHub removes the JIT runner |

## Interfaces / Schema

```rust
// crates/config/src/labels.rs
pub const MAX_LABEL_LEN: usize = 256;
pub const NO_DEFAULT_LABELS_WITHOUT_LABELS: &str =
  "Disabling default labels via --no-default-labels without specifying --labels is not supported";
pub fn default_labels() -> Vec<String>;            // [self-hosted, runner_os(), runner_arch()]
pub fn resolve_labels(user: &[String], no_default_labels: bool, defaults: &[String])
  -> Result<Vec<String>, shared::RunnerError>;     // Err = RunnerError::Config

// crates/config/src/config.rs — RunnerRegistrationConfig
#[serde(default)]
pub ephemeral: bool,                               // after runner_group, before [runtime]

// crates/wire/src/net/auth.rs — exchange_token: 4xx + {"error":"invalid_client"}
//   → RunnerError::Auth("…the runner registration no longer exists on GitHub (OAuth invalid_client)")

// crates/listener/src/loop_decision.rs
pub enum EphemeralAction { Unregister, Exit(i32), BackoffRetry }
pub fn next_ephemeral_action(outcome: &Result<Option<Conclusion>, RunnerError>,
  once: bool, cancelled: bool, pending_remove: bool) -> EphemeralAction;
```

CLI (`register`): `--labels <LABELS>` (now additive), `--no-default-labels`,
`--ephemeral`. `status` prints `mode:` `ephemeral (one job, then unregister)`
or `persistent (re-mints after each job)`.

`config.toml`:

```toml
labels       = ["self-hosted", "Linux", "X64", "gpu"]
runner_group = "Default"
ephemeral    = true
```

Exit codes for `run` on an ephemeral registration: `0` job ran and cleanup
done (any job conclusion, like upstream and `--once`), or shut down before a
job; `1` dead JIT (`Auth`) or `--once` listener error; `2` unregister failed
(state kept) or fatal setup; `5` session conflict.

## Failure modes and edge cases

| Input / state | Observable behavior |
| --- | --- |
| `--no-default-labels` without `--labels` (or only empty entries) | exit 2, upstream message, no HTTP request, no files written |
| `--labels ''` / `a,,b` | empty entries dropped; `--labels ''` alone = defaults only |
| `--labels gpu,GPU,self-hosted` | `[self-hosted, Linux, X64, gpu]` (first spelling wins) |
| label > 256 chars | exit 2 naming the limit, no request |
| ephemeral job succeeds / fails / is cancelled | job reported normally; one acquire; no re-mint POST; DELETE once; state removed; exit 0 |
| SIGINT/SIGTERM before any job | exit 0; config + credentials kept; no DELETE; next `run` reuses the registration |
| transient listener error before a job | backoff, retry the same JIT; no re-mint |
| JIT dead / runner removed on GitHub before a job (`Auth`) | exit 1; state kept; message names `toolu-runner remove` |
| DELETE → 404 and lookup finds no runner | counts as unregistered; state removed |
| DELETE → 5xx / network / 401 | exit 2; state kept; stderr names `toolu-runner remove`; never prints "unregistered" |
| no token available after the job | state removed; message says GitHub-side removal not verified |
| org-level ephemeral registration | as "no token": unverified message, state removed |
| `install-service` on an ephemeral config | exit 2 with the reason; `--remove` still works |
| older `config.toml` without `ephemeral` | loads as persistent; behavior unchanged |

**Retry boundaries.** Before a job, transient listener errors retry forever
with the loop's jittered 1 s → 60 s backoff on the same JIT config (the
registration is unused, so nothing leaks while it waits). After a job, the
GitHub unregister is attempted exactly once per `run` (30 s request timeout,
404 confirmed by one lookup); its retry is the operator's
`toolu-runner remove`, which keeps the same unregister-first contract. A
registration is never deleted locally while a retryable unregister error
stands, so the `runner_id` handle is never lost. The no-token and org-URL
paths delete local state without a GitHub call; they rely on GitHub removing a
used JIT runner itself, which AC-10 verifies live, and the output says the
removal was not verified by toolu.

## Acceptance criteria

- **AC-1:** `register --no-default-labels --labels gpu,GPU,,tpu` sends and
  persists exactly `[gpu, tpu]`; `register --labels gpu` sends and persists
  `[self-hosted, <runner_os>, <runner_arch>, gpu]`; `register --labels ''`
  sends and persists exactly the three defaults.
- **AC-2:** `register --no-default-labels` without labels, or with a label over
  256 characters, exits 2 with the documented message before any HTTP request
  and writes no files.
- **AC-3:** For the live-probed inputs, the label set `resolve_labels` produces
  equals the set GitHub stored, case-insensitively.
- **AC-4:** `register --ephemeral` persists `ephemeral = true`; without it the
  key is `false`; a config without the key loads as `false`.
- **AC-5:** `run` (no `--once`) on an ephemeral registration whose job
  succeeds, fails, or is cancelled acquires exactly one job, issues no
  `generate-jitconfig`, issues one DELETE, removes `config.toml` and
  `credentials.json` (keeps `_diag/`), and exits 0.
- **AC-6:** SIGTERM before any job exits 0 with config and credentials kept and
  no DELETE; a second `run` on the same registration then runs its job and
  cleans up.
- **AC-7:** A failing DELETE (HTTP 500) exits 2, keeps config and credentials,
  and names `toolu-runner remove`; a 404 confirmed by an empty lookup removes
  the state; with no token the state is removed and the output says removal
  was not verified.
- **AC-8:** `next_ephemeral_action` returns the decision table above for every
  row.
- **AC-9:** `install-service` (default and `--print`) refuses an ephemeral
  registration; `status` reports the mode.
- **AC-10:** On github.com, a real ephemeral toolu registration runs one
  dispatched job, exits 0, and `GET /actions/runners/{id}` then returns 404;
  a `--no-default-labels` registration shows exactly the requested labels.
- **AC-11:** `./tools/check.sh all` passes.
- **AC-12:** `run` on an ephemeral registration whose runner was already
  removed on GitHub before any job (the token exchange answers GitHub's
  captured 400 `invalid_client`)
  exits 1, keeps config and credentials, issues no DELETE and no
  `generate-jitconfig`, and names `toolu-runner remove` on stderr.

## Acceptance evidence

| AC | Real input | Expected | Boundary / failure | Check |
| --- | --- | --- | --- | --- |
| AC-1 | real `toolu-runner register` binary (cases `gpu`; `gpu,GPU,,tpu` with `--no-default-labels`; `''`) against a local HTTP endpoint at the GHES `api/v3` path; the request body is recorded | exact `labels` arrays in the body and in `config.toml` | duplicates, empty entries, case | `cargo test -p toolu-runner --test register_labels_test` |
| AC-2 | same binary | exit 2, message, zero requests, no files | no labels; 257-char label | same suite |
| AC-3 | committed live capture `crates/config/tests/fixtures/jit_labels_probe.json` (requests + GitHub's stored labels, 2026-10-11) | set equality | empty list (422 recorded), case duplicates | `cargo test -p config --test labels_test` |
| AC-4 | binary `register --ephemeral`; a pre-#92 TOML text | `ephemeral = true` / `false` | missing key | `register_labels_test`, `labels_test` |
| AC-5 | real binary `run`, real JIT envelope with RSA keypair, captured job message (`fixtures/job_message.json`) running real shell steps `true` / `exit 3` / `sleep 30`+SIGTERM, against a local broker + API endpoint | request counts, files, exit 0, CompleteJob conclusion | three conclusions | `cargo test -p toolu-runner --test ephemeral_run_test` |
| AC-6 | same harness, broker with no job; then a job | exit 0, state kept, 0 DELETE; second run cleans | restart before first job | same suite |
| AC-7 | same harness, DELETE 500 / 404 + empty list / no token | exit codes, kept or removed files, stderr text | three failure shapes | same suite |
| AC-8 | pure function | table rows | every precedence row | `cargo test -p listener --test loop_decision_test` |
| AC-9 | binary `install-service --print`, `status` on a persisted ephemeral config | exit 2 + reason; `mode:` line | `--remove` still allowed | `cargo test -p toolu-runner --test install_service_test`, `cli_test` |
| AC-10 | github.com, this repo, `post-results-live.yml` / `noop-live.yml` dispatched to a unique-label toolu runner | run conclusion, toolu exit 0, runner 404 afterwards; label list | failure conclusion | manual live lane, recorded run ids in `docs/test-coverage.md` |
| AC-11 | whole workspace | green | — | `./tools/check.sh all` |
| AC-12 | same harness as AC-5, token exchange replaying the live 400 `invalid_client` capture; the capture also replayed as raw HTTP to `exchange_token` | exit 1, files kept, 0 DELETE, 0 mint, stderr hint; `RunnerError::Auth` | other 400 error / non-JSON 400 stay `Protocol`, 500 stays `Network` | `ephemeral_run_test`; `cargo test -p wire --test oauth_invalid_client` |

The local broker/API in AC-1/5/6/7 is an HTTP stand-in for GitHub (as in the
existing `remove_unregister_test` and `session_conflict_e2e_test`); it proves
toolu's request/lifecycle wiring. AC-3 and AC-10 carry the server-owned facts
(stored labels, runner removal) from real GitHub.

**Live probe already recorded (2026-10-11, this repo):** `generate-jitconfig`
with `labels: ["toolu-92-probe-only"]` → runner 163 stored exactly
`["toolu-92-probe-only"]` (no hidden `self-hosted`); `labels: []` → HTTP 422
"1 item required"; `labels: ["toolu-92-dup", "TOOLU-92-DUP", "self-hosted"]` →
runner 164 stored `["self-hosted", "toolu-92-dup"]`. Both runners were deleted.

## Documentation impact

- `README.md`: CLI flags table (`--labels`, `--no-default-labels`,
  `--ephemeral`), the `config.toml` example, mode notes.
- `crates/toolu-runner` `--help` text (`register`, `run`, `install-service`).
- `AGENTS.md`: `cli.rs` / `run_cmd.rs` / `config` module bullets, the
  always-online run-loop rule (ephemeral exception), new `config/labels.rs`
  and `toolu-runner/src/ephemeral.rs`.
- `docs/known-bugs.md` is not affected (the deleted-registration retry was
  not tracked there); the fix is recorded in the #92 coverage section.
- `docs/test-coverage.md`: #92 section mapping every AC and 92-S1..S4 to
  evidence, with Linux/macOS and github.com/GHES applicability.
- `CHANGELOG.md` is generated by git-cliff — not edited by hand.

## Open Questions

None blocking. GHES and macOS live runs are not available to this worker and
are recorded as **unverified** in `docs/test-coverage.md` (owner: epic #67
acceptance lanes, #103).

## Spec review (2026-10-11)

Round 1 — Needs changes:

```
Acceptance criteria: 🔴 blocker: 92-S3 "already-removed registration" had a failure-table row but no AC or runnable check. Added AC-12 + evidence row.
Acceptance criteria: 🟡 should-fix: AC-1 did not cover `--labels ''` (empty input). Added the defaults-only case to AC-1 and its evidence row.
```

Round 2 — every section filled; each AC (1–12) has a real input, expected
result, boundary and runnable check; 92-S1..S4 map to AC-1/2/3, AC-5/10,
AC-6/7/12, and the mode-contract table; scope matches the issue (label
spelling and the `install-service` refusal follow from the parity goal and
the "no contradictory lifecycle modes" requirement); no blocking question.

**Status:** Approved

Amendment (execution, 2026-10-12): the live lane showed AC-12's premise
(401) was wrong — GitHub sends 400 `invalid_client`. The `invalid_client`
classification above was added and AC-12 now uses the live capture
(Jev: in scope 0.91, safe for the default loop 0.90). Re-approved.
