# Release profile: opt-level "s" + fat LTO, gated on benchmarks — Design

> Historical implementation record. Current behavior is documented in
> [architecture](../architecture.md); acceptance status and remaining gaps are
> tracked in [test coverage](../test-coverage.md) and the
> [compatibility matrix](../compatibility-matrix.md).

**Date:** 2026-10-14   **Status:** Approved   **Author:** Claude   **Topic:** Issue #112 (epic #106)

## Problem

Release `toolu-runner` / `toolu-daemon` are built at `opt-level = 3` with thin
LTO. Measured on macOS arm64 at `2ca6459`, `"s"` + fat LTO is 29.9% / 21.5%
smaller. Size optimization can slow the runner's hot paths (secret masking,
BLAKE3/FastCDC chunking, expression interpolation), so the change must be
decided by measurement, not assumed.

## Non-Goals

1. Per-crate `opt-level = 3` overrides (may not survive fat LTO; add only if a measured regression needs one — none planned).
2. Changing `codegen-units`, `strip`, `panic`, the dev/test profiles or the Dockerfile.
3. `opt-level = "z"`, PGO, or dependency pruning (the linker-map findings are out of scope).
4. Producing macOS arm64 numbers on this Linux host: those are cited from the issue and labelled as such.

## Architecture

Decision rule, fixed before measuring: candidate = `"s"` + fat; baseline =
current `3` + thin; fallback = `3` + fat. Measure all three on one host, one
tree, separate target dirs, via `CARGO_PROFILE_RELEASE_OPT_LEVEL` /
`CARGO_PROFILE_RELEASE_LTO` (the `bench` profile inherits `release`, so the
benches run under the same settings). **Regression threshold: a hot-path
metric more than 10% worse than baseline** (criterion mean time for the two
criterion benches; MB/s for cache save and restore). Candidate passes iff no
metric exceeds the threshold; otherwise the fallback is adopted if it passes,
and the reason is recorded. Wall-clock noise on a shared host is handled by
3 repetitions of the cache bench (median) and criterion's own statistics.

New measurement: `crates/cache/benches/cas_roundtrip.rs` (plain `harness =
false` main, no new dependency) — builds a 512 MB directory of 64 files
(random + repetitive), assembles it into the staged blob a cache save
produces, `CasStore::ingest` (save), then restores all bytes through
`CasStore::read_range` (BLAKE3-verified), checks length and hash, and prints
MB/s.

Evidence and decision are recorded in `docs/release-profile.md`; a shell test
pins the shipped `[profile.release]`.

## Interfaces / Schema

- `Cargo.toml` `[profile.release]`: `opt-level` and `lto` per the decision; `codegen-units = 1`, `strip = "symbols"`, `panic = "abort"` unchanged.
- `cargo bench -p cache --bench cas_roundtrip` (env `CAS_BENCH_MB`, default 512) prints one line: `cas_roundtrip bytes=… chunks=… save_s=… save_MBps=… restore_s=… restore_MBps=…`.
- `scripts/test/release_profile_test.sh` exits non-zero if `[profile.release]` does not match the documented decision.

## Failure modes and edge cases

- A restore that returns wrong bytes or length aborts the bench (assert), so a throughput number never comes from a corrupt restore.
- A candidate failing the threshold: fallback adopted, reason in the doc; if the fallback also fails, the profile stays unchanged and the doc records why.
- `cas_roundtrip` is `test = false`, so `cargo test` / nextest never run the 512 MB pass.
- A build failure under the new profile fails the measurement script and the gate.

## Acceptance criteria

- **AC-1:** Given the committed `secret_masker` and `interpolate` criterion benches, the PR records before (3+thin) and after (adopted profile) criterion means, and states the 10% threshold.
- **AC-2:** Given `cas_roundtrip` over a 512 MB directory, the PR records before/after save and restore MB/s, and the restore is hash-verified equal to the source.
- **AC-3:** Given both release binaries, the PR records before/after sizes and release build wall time on Linux amd64 (measured) and macOS arm64 (cited from the issue), for baseline, candidate and fallback.
- **AC-4:** `Cargo.toml` `[profile.release]` equals the adopted profile and `scripts/test/release_profile_test.sh` passes against it and fails on a mutated copy.
- **AC-5:** `./tools/check.sh all` passes.

## Acceptance evidence

- AC-1: real fixtures in `crates/*/benches/fixtures`; `cargo bench -p shared --bench secret_masker`, `-p execution --bench interpolate` under both profiles; results table in `docs/release-profile.md`.
- AC-2: generated 512 MB tree; `cargo bench -p cache --bench cas_roundtrip` ×3 per profile; boundary: assertions on hash/length.
- AC-3: `stat` of `target-*/release/toolu-runner|toolu-daemon`, `date`-timed cold `cargo build --release`.
- AC-4: `bash scripts/test/release_profile_test.sh`; negative: run against a sed-mutated copy of `Cargo.toml` and expect failure.
- AC-5: `./tools/check.sh all`.

## Documentation impact

New `docs/release-profile.md` (decision, threshold, tables, reproduction commands); `crates/cache/src` has no new module so no src README; `README.md`/`AGENTS.md` unaffected beyond a one-line pointer in AGENTS.md References.

## Open Questions

- Threshold of 10%: decided by the worker (owner: worker), non-blocking; it is stated in the PR as the issue requires.

## Spec review

No Jev call: no semantic ambiguity — thresholds, sizes and throughput are numeric and the checks deterministic.
Findings: none blocking. 🔵 AC-3 macOS arm64 numbers are cited, not re-measured (stated as non-goal 4). 🔵 AC-1 "after" is the adopted profile, so if fallback is adopted the candidate's numbers are still recorded for the rejection reason.
