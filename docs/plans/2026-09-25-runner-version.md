# Runner identity and updates — Implementation plan

**Date:** 2026-09-25   **Status:** Approved   **Spec:** docs/specs/2026-09-25-runner-version-design.md   **Topic:** Issue #78

## Evidence and approach

Reuse protocol session builder, wire message transports and #77 idle/busy control
handlers. Advertise the pinned official 2.337.0 baseline consistently; keep Cargo
product identity separately observable. Refresh requests warn/skip, with operator
maintenance as the only update path. Real outgoing HTTP request capture proves
serialization; real GitHub jobs alone establish observed queue acceptance.

## Workstream summary

Reproduce the mismatch, unify identity, pin both update-control paths, document
operator maintenance and measured service acceptance, then pass the full gate.

## Steps (machine-readable)

```json
[
  {
    "id": "identity",
    "title": "Unify session, idle/busy poll and acknowledgement identity",
    "ac_refs": ["AC-1"],
    "paths": ["Cargo.toml", "Cargo.lock", "crates/protocol", "crates/wire", "crates/listener", "crates/shared", "crates/toolu-runner/tests/gh_compat_poll_cursor.rs", "crates/toolu-runner/tests/net_test.rs", "crates/toolu-runner/tests/cli_test.rs"],
    "input": "Actual outgoing reqwest requests captured over loopback from the production transports; exact 2.337.0 JSON/query values, cursor 0/99, disableUpdate=true and disconnect errors; real CLI version output",
    "check": "cargo test -p wire --test runner_version && cargo test -p toolu-runner --test gh_compat_poll_cursor --test net_test"
  },
  {
    "id": "refresh-policy",
    "title": "Verify refresh skip and warning in idle and busy production handlers",
    "ac_refs": ["AC-2"],
    "depends_on": ["identity"],
    "paths": ["Cargo.toml", "Cargo.lock", "crates"],
    "input": "Committed broker envelope with labelled RunnerRefresh/AgentRefresh variations, opaque body, repeated ID and following real job shape; fixed warning, no cancellation, continue decisions",
    "check": "cargo test -p listener --lib runner_update_policy"
  },
  {
    "id": "live-docs",
    "title": "Document update ownership and record real service acceptance evidence",
    "ac_refs": ["AC-3", "AC-4"],
    "depends_on": ["refresh-policy"],
    "paths": ["README.md", "AGENTS.md", "docs", "scripts/test/runner_version_evidence_check.py", "scripts/test/test_runner_version_evidence_check.py", ".github/workflows/noop-live.yml"],
    "input": "GitHub update policy, pinned official source and matching real toolu/reference workflow runs; evidence JSON records exact source, platform and conclusions; GHES explicitly unverified",
    "check": "python3 scripts/test/runner_version_evidence_check.py && python3 -m unittest discover -s scripts/test -p test_runner_version_evidence_check.py && git diff --check"
  },
  {
    "id": "gate",
    "title": "Pass unchanged repository gate on the final source",
    "ac_refs": ["AC-5"],
    "depends_on": ["live-docs"],
    "paths": ["."],
    "input": "Final workspace, committed scenario coverage map and operator documentation",
    "check": "./tools/check.sh all && git diff --check"
  }
]
```

## Critical files

Create `crates/protocol/src/runner_version.rs`, `crates/wire/tests/runner_version.rs`,
`crates/listener/src/tests/runner_update_policy.rs`, `docs/runner-updates.md`,
`docs/runner-version-evidence.json`, `scripts/test/runner_version_evidence_check.py`.
Modify protocol module declaration/session builder, wire messages, listener
handler/job_lifecycle/broker_message, existing poll tests and test README; update
README, AGENTS, architecture and test coverage. The live evidence checker must
fail if required GitHub.com evidence is absent, never turn missing access into a
passing skip. Transport failures are asserted as errors, not fabricated success.

## Verification

Use real HTTP requests, actual production idle/busy handlers and the unchanged
repository gate. For the live lane, register isolated toolu and official 2.337.0
JIT instances with the existing noop workflow label; dispatch the same workflow
revision sequentially and record actual GitHub completion/runner identities.
Use only this worker's instances, credentials kept outside the repository.
GHES remains explicitly unverified, without claiming eligibility there. Missing
GitHub.com execution capability blocks AC-4 and is reported via needs-human.

Before delivery fetch/rebase origin/main if it moved and re-run the gate. The
brief authorizes scoped commit/push/PR and babysit, never merging. After the
scoped commit, run final `plan-ledger.sh run <plan> --verify`, local push review
and verdict checks; require fresh green state before push. PR targets main,
starts with the two issue/epic references and includes exact verification and
unverified limits. Run babysit to strict clearance and report ready.

Plan review: approved; all ACs mapped, ordering/path declarations checked.

## Deviations

The macOS loader stalled both rebuilt tests and CLI at `_dyld_start` (sample
`/tmp/issue78-version-sample.txt`); copying the binary to internal storage also
timed out. Run the same commands on an isolated Linux ARM64 source snapshot in
`toolu-72-gate-tools:local`, with a worker-owned target volume. Use equivalent
Linux hosts for both live runners. This does not count macOS tests as passing.

The Linux gate uses `CARGO_PROFILE_DEV_DEBUG=0` and
`CARGO_PROFILE_TEST_DEBUG=0` after the shared VM exhausted disk while linking
debug symbols; `CARGO_INCREMENTAL=0` prevents the worker cache from regrowing. No check/lint/test is disabled. The local ignored ledger copy
wraps the same Rust commands in Docker using `/tmp/issue78-check.sh`; that
adapter copies this worktree with checksum comparison and fresh mtimes into
the worker-owned source mirror and uses only `toolu78_target`. The committed
commands remain directly runnable on a normal Linux host.

Pre-push retains the repository Lefthook hooks. A temporary local config copies
them verbatim except that `full-check` invokes the Linux adapter above; its
resolved configuration is checked with `lefthook dump`. This keeps the full
unchanged gate active on push without invoking the stalled macOS test loader.
