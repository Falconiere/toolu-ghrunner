# Shell templates — Plan

**Date:** 2026-09-26   **Status:** Approved   **Spec:** docs/specs/2026-09-26-shell-templates-design.md   **Topic:** #80

## Evidence and approach

The issue and cab9d1c upstream contract require explicit versus default arguments,
placeholder validation, executable preservation and PowerShell fixups. Existing
script.rs and composite_shell.rs duplicate selection; container composites already
use ScriptHandler. Reuse shlex and captured jobs, preserve streaming/cancellation.

## Workstream summary

Failing real-tool probes → shared command and composite adapter → replay and
platform evidence → docs and full gate → authorized feature PR and babysit.

## Steps (machine-readable)

```json
[
  {
    "id": "shell",
    "title": "Reproduce and implement one shared shell contract",
    "ac_refs": [
      "AC-1",
      "AC-2",
      "AC-3",
      "AC-4"
    ],
    "check": "cargo test -p execution --test shell_templates_test && cargo test -p execution --test shell_templates_replay_test",
    "paths": [
      "crates/execution/src",
      "crates/execution/tests",
      "crates/toolu-runner/tests/fixtures",
      "Cargo.lock"
    ],
    "input": "Real bash/sh/perl, captured incoming_contexts_matrix_0 job, explicit and default shells, scoped composite manifests; python/pwsh installation checks separately"
  },
  {
    "id": "evidence",
    "title": "Run interpreter and Docker probes and document measured support",
    "depends_on": [
      "shell"
    ],
    "ac_refs": [
      "AC-1",
      "AC-3",
      "AC-4",
      "AC-5"
    ],
    "check": "cargo test -p execution --test shell_templates_test -- --include-ignored && bash scripts/test/shell_templates_linux.sh && python3 scripts/test/shell_templates_evidence_check.py",
    "paths": [
      "README.md",
      "docs/test-coverage.md",
      "crates/execution",
      "crates/shared",
      "crates/toolu-runner/tests/fixtures",
      "scripts/test/shell_templates_evidence_check.py",
      "scripts/test/shell_templates_linux.sh",
      ".github/workflows/shell-templates-80.yml",
      "Cargo.lock"
    ],
    "input": "Actual recorded host, interpreter, Linux-container results and pinned source contract; unverified live lanes explicit"
  },
  {
    "id": "gate",
    "title": "Full gate and delivery verification",
    "depends_on": [
      "evidence"
    ],
    "ac_refs": [
      "AC-5"
    ],
    "check": "./tools/check.sh all",
    "paths": [
      "."
    ],
    "input": "Entire workspace and repository guardrails"
  }
]
```

## Critical files

- crates/execution/src/execution/handlers/{shell_command,script}.rs and handlers.rs
- crates/execution/src/execution/composite_shell.rs
- crates/execution/src/execution/handlers/README.md and execution/README.md
- crates/execution/tests/shell_templates{_test,_replay_test,_container_test}.rs
- crates/execution/tests/shell_templates_evidence.json
- scripts/test/shell_templates_{evidence_check.py,linux.sh}
- README.md, docs/test-coverage.md, .github/workflows/shell-templates-80.yml

## Verification

First observe default pipeline/custom/unknown-shell regressions. Run real process
probes including path/argument spaces, absent bash, nonexecutable entries, invalid
configuration and native failures. Replay captured jobs with labelled script and
shell/default substitutions through Runner::execute_job and real composite actions.
Run installed Python/PowerShell explicitly and Linux container probes with Docker;
missing tools or live infrastructure are unverified, never passing. Update coverage
and evidence artifact with commands and source provenance. All AC refs must be
fresh-green via plan-ledger run --verify. Commit only scoped issue files, review
committed diff with toolu-review, require green verdict, fetch/rebase and rerun gate
if main moves, then push authorized feature branch and create PR with required
Closes/Part-of body. Run pr-babysit to strict clearance; never merge.

## Plan review

Approved after direct spec/ledger comparison: all AC references resolve, checks
are runnable, production/test/doc paths are included, dependencies are acyclic,
and delivery is authorized. Jev sequence assessment was uncertain (0.46); direct
comparison to the approved spec resolves scope without expanding behavior.

## Deviations

Preserve public handler callers that omit PATH by falling back to runner PATH
through execution::config, matching old process lookup and upstream. Explicit
PATH values remain authoritative. Jev selected fallback (1.0); re-reviewed spec
and plan approve this bounded compatibility correction.

Evidence check requires installed python/pwsh on PATH and real Docker, in addition
to validating the documentation map. This prevents ignored interpreter tests
from stamping the acceptance step green. Review approves this stronger check.

Full gate exposed macOS ps command-path observations in the existing defaults
replay. Normalize the observed command to basename in that replay only; captured
fixture, shell identity, and cwd assertions stay intact. Include
crates/execution/tests/job_message_run_defaults_test.rs in the scoped change and
run that regression explicitly. Pinned action_yaml.json requires composite
shell, so no omitted-composite fallback is added.
