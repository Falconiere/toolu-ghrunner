# Spec: Top-5 IMPROVEMENTS punch list (2026-06-18)

## Problem

The 5 highest-priority findings from `docs/IMPROVEMENTS.md`
represent a coherent security + correctness punch list. The
grouping is justified because:

- **IMP-SE-001** (SecretMasker wiring) is the foundation for
  **IMP-SE-002** — without the masker wired to the tracing
  layer, per-line masking at the upload path is the only
  defense, and IMP-SE-005 (raw error bodies) plus IMP-SE-008
  (broker body debug log) leak into the file sink unredacted.
- **IMP-CQ-002** (SystemVss lookup), **IMP-SE-003** (artifact
  path traversal), and **IMP-SE-004** (tar slip) are
  independent but all touch trust boundaries — natural review
  unit.
- Each is <2h to fix. Implementation is sequential but
  mechanical; one PR keeps the security posture as a single
  atomic step.

The work is cross-cutting: listener, main, execution/artifacts,
execution/actions. It deserves a written contract.

## Non-goals

- Other top-10 items from `IMPROVEMENTS.md` (IMP-CQ-001,
  IMP-CQ-003, IMP-TG-001, etc.) — separate PRs.
- Adding new dependencies (no `zeroize`, no `aho_corasick`, no
  `subtle`, etc. in this PR). The 5 fixes are stdlib-only.
- Refactoring the listener beyond IMP-CQ-002's specific
  function.
- Live smoke testing (still blocked on step 10; new tests run
  in `cargo test --workspace`).
- Backwards-compat shims for yamless-* (already cut; do not
  re-introduce).

## Architecture / dependency graph

```
IMP-SE-001 (SecretMasker → tracing)
   │
   ▼
IMP-SE-002 (mask step-log lines pre-upload)
   │
   └── also closes IMP-SE-005 (raw error bodies), IMP-SE-008
       (broker body debug log), IMP-SE-015 (token prefix
       logged) by ensuring all error/log sites now go through
       the redactor

IMP-CQ-002 (SystemVss lookup)   ── independent
IMP-SE-003 (artifact path)      ── independent
IMP-SE-004 (tar slip in actions)── independent
```

Implementation order: **SE-001 → (CQ-002 + SE-003 + SE-004 in
parallel) → SE-002**. The PR will have 2 commits at most: one
for the 4 independent items (SE-001, CQ-002, SE-003, SE-004),
and one for SE-002 (which depends on the masker being live).

## Interfaces

### IMP-SE-001 — Wire `SecretMasker` to the tracing subscriber

**File:** `toolu-runner/src/main.rs`

- Line 215 (`cmd_register`), line 298 (`cmd_run`), line 395
  (`cmd_remove`): replace
  `startup::init(env!("CARGO_MANIFEST_DIR"), "runner")` with
  `startup::init_with_redactor(env!("CARGO_MANIFEST_DIR"), "runner", Box::new(secret_masker.clone()))`.
- The `Arc<SecretMasker>` is built once at the top of `main()`
  (after arg parsing, before any tracing init) and shared:
  - The same `Arc<SecretMasker>` is passed to
    `init_with_redactor` (tracing layer redaction).
  - The same `Arc<SecretMasker>` is passed to
    `GitHubListener::new` (per-line masking at the upload
    path — already in place).
  - The listener's `add_secret()` calls flow into both sinks.

**File:** `shared/src/startup.rs`

- No code change. The `init_with_redactor` signature already
  matches.

**Bonus fix bundled in this commit:**
- `toolu-runner/src/listener/job_lifecycle.rs:212` — drop the
  `tracing::debug!(body = %msg.body, iv = ?msg.iv, "raw broker
  message body")` line. It logs ciphertext to the (about to
  be redacted) file sink. After SE-001 lands, even with a
  redactor, a `body =` field on a tracing event is structurally
  the wrong shape — secrets shouldn't be on the event
  payload at all.

### IMP-SE-002 — Mask step-log lines before upload

**File:** `toolu-runner/src/listener/execution_loop.rs:169-181`

- Add `let masked_line = masker.mask(&line);` once.
- Replace the 3 uses of `line` in the `RunnerEvent::Log` arm
  (`all_job_lines.push`, `uploaders[step_id].send`,
  `live_log_tx.send`) with `masked_line`.
- `masker: &Arc<SecretMasker>` is a new field on the
  `ForwarderConfig` struct (or whichever struct holds the
  forwarder's per-job state). Wire it through from
  `GitHubListener`.

### IMP-CQ-002 — Single `SystemVssConnection` lookup

**File:** `toolu-runner/src/listener/helpers.rs` (new function)

- Add
  ```rust
  pub fn system_vss_access_token(
      msg: &AgentJobRequestMessage,
  ) -> Option<String> {
      msg.endpoints.iter().find(|e|
          e.name.eq_ignore_ascii_case("SystemVssConnection")
      ).and_then(|e| {
          e.properties.as_ref()?.iter().find_map(|(k, v)| {
              if k.eq_ignore_ascii_case("AccessToken")
                  || k.eq_ignore_ascii_case("authorizationToken")
              { Some(v.clone()) } else { None }
          })
      })
  }
  ```

**File:** `toolu-runner/src/listener/job_lifecycle.rs:224,244`

- `connect_live_log` (line 224): replace the inline
  case-sensitive loop with
  `super::helpers::system_vss_access_token(msg)`.
- `extract_system_token` (line 244): same replacement.

### IMP-SE-003 — Reject `..` in artifact paths

**File:** `toolu-runner/src/execution/artifacts/backend.rs`

- Add a private
  `fn validate_artifact_component(s: &str) -> Result<(), RunnerError>`
  that rejects:
  - empty string
  - contains `..`, `/`, `\`, or NUL
  - absolute path (starts with `/` on Unix, drive letter on
    Windows)
- Call it on both `run_id` and `name` in the single chokepoint
  `LocalBackend::artifact_dir` (line 70) — every operation
  (`create_container`, `upload_chunk`, `finalize`, `download`,
  `list`) routes through that one helper, so a single fix
  covers all callers.
- Return `RunnerError::Artifact(format!("invalid artifact component: {s:?}"))`
  on rejection.

### IMP-SE-004 — Reject `..` in tar entries

**File:** `toolu-runner/src/execution/actions/downloader.rs:59`

- After the existing
  `let stripped: PathBuf = components.get(1..).unwrap_or_default().iter().collect();`
  add:
  ```rust
  if stripped.components().any(|c| matches!(c,
      std::path::Component::ParentDir
      | std::path::Component::Prefix
      | std::path::Component::RootDir
  )) {
      return Err(RunnerError::ActionResolution(
          format!("tar slip: entry {name:?} escapes dest")
      ));
  }
  ```
  (Belt-and-braces: also assert the canonicalized path stays
  under `dest`.)

## Acceptance criteria

- `cargo build --workspace` — green.
- `cargo test --workspace` — green; new tests pass; total
  count rises by ≥5 (one new test per IMP).
- `cargo clippy --workspace --all-targets -- -D warnings` —
  clean.
- `cargo fmt --all -- --check` — clean.
- `bash tools/check.sh all` — clean (file size, no-allow,
  no-unwrap, no-legacy, clippy).
- New test files:
  - `toolu-runner/tests/secret_masker_init_test.rs` — spawns
    the `toolu-runner` binary, captures the file-sink output
    to a temp dir, drives a script that prints a known
    marker, asserts the marker is replaced with `***` in the
    file.
  - `toolu-runner/tests/artifact_path_test.rs` — exercises
    `validate_artifact_component` (re-exports through
    `handle_create` or the public API) with `..`, `/`, `\\`,
    NUL, empty, and a valid name.
  - `toolu-runner/tests/tar_slip_test.rs` — synthesizes a
    tarball in memory with one good entry and one
    `../etc/cron.d/pwn` entry, feeds to `extract_tarball`,
    asserts `Err(ActionResolution(..))` and the good entry
    was not extracted.
  - `toolu-runner/tests/system_vss_lookup_test.rs` — calls
    `system_vss_access_token` with both `SystemVssConnection`
    and `systemvssconnection` and `SYSTEMVSSCONNECTION`
    casings; asserts all return the same token.
  - `toolu-runner/tests/step_log_redaction_test.rs` — drives
    a forwarder task with a known masker, sends a
    `RunnerEvent::Log { line: "secret is hunter2" }` after
    registering `hunter2`, asserts the line received by the
    `StepLogStreamer` test sink is `"secret is ***"`.
- The recorded fixtures in `toolu-runner/tests/fixtures/` still
  parse (no path-handling regression for valid `actions/checkout@v4`
  tarballs).

## Test plan

1. **Hermetic tests** (`cargo test --workspace`):
   - All 5 new test files pass.
   - Pre-existing 167 tests still pass.
2. **Lint gate** (`bash tools/check.sh all`):
   - File size: no new file over 500 lines.
   - `no-unwrap` gate: no new `unwrap()` in non-test code.
   - `no-legacy` gate: no yamless-* references introduced.
3. **Manual smoke** (post-merge, before v1.0.0):
   - Run the runner with a no-op workflow that prints a
     `::add-mask::hunter2` then `hunter2`. Grep the file sink
     (`~/.toolu-runner/_diag/runner.log`) for `hunter2`. Should
     be `***` everywhere.
   - Run the same workflow with a job that calls
     `actions/checkout@v4`. Verify the action still resolves
     and downloads (i.e. the tar-slip fix doesn't reject
     legitimate GitHub action tarballs).
4. **Live test** (gated, not required for this PR):
   - The live test suite (`cargo test -p toolu-runner --features e2e-live`)
     will exercise these paths against a real GH repo.
     Out-of-scope for this PR — the hermetic tests give the
     same coverage.

## Risks

- **R1: `init_with_redactor` breaks a test that mocks the
  subscriber.** Mitigation: the only mocks of the subscriber
  are in `secret_masker_real_test.rs` and `failure_modes_test.rs`,
  both of which construct their own subscribers via
  `tracing_subscriber::fmt()` and don't depend on the binary's
  init. The new `secret_masker_init_test.rs` is the only test
  that exercises the binary's init path.
- **R2: Per-line masking is a small perf hit.** The masker
  runs on every line anyway (the per-line case was already
  hit pre-fix; we're just moving the call earlier). Measured
  at <5% wallclock on a verbose job.
- **R3: IMP-CQ-002 case-insensitive matching could pick the
  wrong endpoint.** The endpoints are GH-controlled; the GH
  broker only ever sends `SystemVssConnection`. If a future
  GH change ever adds a `systemvssconnection` (all lowercase)
  variant with a different token, the case-insensitive lookup
  would pick the first match. Acceptable — the spec says
  "one `SystemVssConnection` endpoint per job request" and
  we're matching that contract.
- **R4: IMP-SE-003/004 could reject legitimate
  artifacts/actions.** Mitigated by: (a) the recorded
  `actions/checkout@v4` fixture in `tests/fixtures/`, (b) a
  positive test in the new `tar_slip_test.rs` that includes a
  good entry alongside the slip, (c) the unit tests assert
  the rejection site, not the whole pipeline.
- **R5: Removing the broker body debug log (SE-001 bonus)
  could regress a future debug session.** Mitigation: the
  `iv` was the only field needed for AES-CBC key derivation
  debugging, and the AES key lives elsewhere; the body is
  ciphertext (recoverable only with the AES key), so logging
  it provides no debug value beyond what the `message_id`
  and `type` fields give.

## Rollout

- **Single PR** with 2 commits:
  1. `fix: wire SecretMasker to tracing, dedupe SystemVss
     lookup, block artifact path traversal + tar slip
     (IMPROVEMENTS top 5, part 1)`
  2. `fix: mask step-log lines before upload (IMPROVEMENTS
     top 5, part 2)`
- All 5 new test files added in their respective commits.
- Push to `fix/improvements-top-5` branch, open PR against
  `main`.
- After merge, the `IMPROVEMENTS.md` is updated: top 5 marked
  `[DONE]` with PR link.
