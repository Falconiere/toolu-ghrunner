# Release profile: opt-level "s" + fat LTO, gated on benchmarks — Plan

> Historical implementation record. Current behavior is documented in
> [architecture](../architecture.md); acceptance status and remaining gaps are
> tracked in [test coverage](../test-coverage.md) and the
> [compatibility matrix](../compatibility-matrix.md).

**Date:** 2026-10-14   **Status:** Approved   **Spec:** docs/specs/2026-10-14-release-opt-level-s-fat-lto-design.md   **Topic:** issue #112 — measured release profile

## Evidence and approach

Inspected `Cargo.toml` `[profile.release]` (line 133), `crates/shared/benches/secret_masker.rs`,
`crates/execution/benches/interpolate.rs` (criterion, `bench` profile inherits `release`),
`crates/cache/src/cas/store.rs` (`ingest`, `read_range`), `.github/workflows/ci.yml`
release-script test list, `scripts/test/dockerfile_test.sh` (mutation-test pattern).
Three profiles (3+thin baseline, s+fat candidate, 3+fat fallback) are measured in separate
target dirs via `CARGO_PROFILE_RELEASE_*`; the decision rule (10% threshold) is in the spec.
Measurement script and raw results are committed in the evidence doc.

## Workstream summary

cache bench → measurements → evidence doc → profile + pin test → CI hook → full gate.

## Steps (machine-readable)

```json
[
  {
    "id": "s1-cache-bench",
    "title": "cas_roundtrip bench saves and restores a generated tree, verifies the restore, and prints MB/s",
    "check": "CAS_BENCH_MB=16 cargo bench -p cache --bench cas_roundtrip 2>/dev/null | grep -Eq '^cas_roundtrip bytes=[0-9]+ chunks=[0-9]+ save_s=[0-9.]+ save_MBps=[0-9.]+ restore_s=[0-9.]+ restore_MBps=[0-9.]+$'",
    "ac_refs": ["AC-2"],
    "paths": ["crates/cache/benches/cas_roundtrip.rs", "crates/cache/Cargo.toml"],
    "input": "generated 16 MB tree through the real CasStore"
  },
  {
    "id": "s2-evidence",
    "title": "docs/release-profile.md records the threshold and before/after tables for criterion, cache, sizes and build time",
    "check": "grep -q '10%' docs/release-profile.md && grep -q 'secret_masker' docs/release-profile.md && grep -q 'interpolate' docs/release-profile.md && grep -q 'cas_roundtrip' docs/release-profile.md && grep -q 'toolu-daemon' docs/release-profile.md && grep -q 'build wall' docs/release-profile.md",
    "ac_refs": ["AC-1", "AC-2", "AC-3"],
    "depends_on": ["s1-cache-bench"],
    "paths": ["docs/release-profile.md"],
    "input": "measured results from the three profile runs"
  },
  {
    "id": "s3-profile-pin",
    "title": "Cargo.toml [profile.release] equals the adopted profile; the pin test passes and catches five mutations",
    "check": "bash scripts/test/release_profile_test.sh",
    "ac_refs": ["AC-4"],
    "depends_on": ["s2-evidence"],
    "paths": ["Cargo.toml", "scripts/test/release_profile_test.sh"],
    "input": "real Cargo.toml and five sed-mutated copies"
  },
  {
    "id": "s4-ci-hook",
    "title": "ci.yml release-script test list runs release_profile_test",
    "check": "grep -q ' release_profile ' .github/workflows/ci.yml && bash scripts/test/ci_workflow_test.sh",
    "ac_refs": ["AC-4"],
    "depends_on": ["s3-profile-pin"],
    "paths": [".github/workflows/ci.yml", "scripts/test/ci_workflow_test.sh"],
    "input": "real ci.yml"
  },
  {
    "id": "s5-docs-pointer",
    "title": "AGENTS.md references docs/release-profile.md",
    "check": "grep -q 'docs/release-profile.md' AGENTS.md",
    "ac_refs": ["AC-4"],
    "depends_on": ["s2-evidence"],
    "paths": ["AGENTS.md"],
    "input": "AGENTS.md"
  },
  {
    "id": "s6-gate",
    "title": "the full quality gate passes",
    "check": "./tools/check.sh all",
    "ac_refs": ["AC-5"],
    "depends_on": ["s1-cache-bench", "s3-profile-pin", "s4-ci-hook", "s5-docs-pointer"],
    "paths": ["Cargo.toml", "crates/cache", "scripts", "docs", ".github/workflows/ci.yml", "AGENTS.md"],
    "input": "whole workspace"
  }
]
```

## Critical files

`Cargo.toml`, `crates/cache/Cargo.toml`, `crates/cache/benches/cas_roundtrip.rs`,
`scripts/test/release_profile_test.sh`, `.github/workflows/ci.yml`, `docs/release-profile.md`, `AGENTS.md`.

## Verification

End to end: `cargo build --release` yields smaller binaries under the adopted profile; benches
show every hot-path metric within 10% of baseline; `./tools/check.sh all` green. Failure path:
the pin test rejects drifted profiles. Docs: `docs/release-profile.md` + AGENTS pointer.

## Plan review

No Jev call: no classification changes step handling. AC-1..AC-5 each map to a step with a runnable check; dependencies are acyclic; s3's expected values change with the measured decision and are set after s2. Status: Approved.
