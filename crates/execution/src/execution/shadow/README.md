# shadow/

**What belongs here:** shadow-mode (approach C) `run:`-step observation —
deterministic workspace fingerprinting and the masked would-hit/false-hit
record it appends. Records only: nothing here ever returns or reuses a step
result.

**What does NOT belong here:** the observer's wiring into the step loop
(`ShadowObserver::pre`/`post` calls around a `run:` step) — that lives in
`execution::steps_runner`, which owns the actual step execution. Any future
real caching layer would be a different module entirely; this one is
diagnostic-only by design.

## Contents

| File | Primary item | Purpose |
| --- | --- | --- |
| `fingerprint.rs` | `fingerprint_dir` | Deterministic BLAKE3 digest of a directory tree's structure + file contents, walked depth-first in byte-sorted name order; symlinks are recorded by target, not followed. |
| `record.rs` | `ShadowRecord` | The masked JSON-line observation record (cmd/env/cwd digests, pre/post fingerprints, `would_hit`/`false_hit`) plus its `StepKey` bundle and digest helpers. |

When you add a file here, add its row above so the index stays current. No
`mod.rs` barrel — declare submodules from the parent file (`src/foo.rs`
declares `mod bar;` for `src/foo/bar.rs`) and import concrete paths.
