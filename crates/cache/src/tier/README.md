# tier/

**What belongs here:** optional cold-storage tiers behind the L1 CAS — today,
the S3-compatible mirror of immutable chunks and manifests.

**What does NOT belong here:** the L1 (hot, local) content-addressed store
itself lives in `../cas/`, which is what calls into this tier on a local miss
or to mirror a new write; the HTTP-facing protocols never talk to this tier
directly.

## Contents

| File | Primary item | Purpose |
| --- | --- | --- |
| `l2.rs` | `L2Tier`, `BlobKind` | S3-backed cold tier mirroring immutable CAS chunks and manifests (never the index) via `opendal`; content-addressed puts are idempotent. |

When you add a file here, add its row above so the index stays current. No
`mod.rs` barrel — declare submodules from the parent file (`src/foo.rs` declares
`mod bar;` for `src/foo/bar.rs`) and import concrete paths.
