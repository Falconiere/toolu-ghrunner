# cas/

**What belongs here:** the content-addressed store (CAS) itself — FastCDC
chunking, BLAKE3-keyed chunk I/O, the ranged-read manifest, the restart-safe
`(scope, version)` index, and garbage collection.

**What does NOT belong here:** the HTTP-facing protocols that read and write
through this store (Twirp v2 in `../twirp/`, legacy REST v1 in `../v1/`, the
Azure-Blob endpoint in `../blob/`); the optional S3 cold tier this store
delegates to lives in `../tier/`, not here.

## Contents

| File | Primary item | Purpose |
| --- | --- | --- |
| `chunk_io.rs` | `Durability`, `write_atomic` | Content-addressed file IO: temp file + optional fsync + atomic rename, verify-on-read. |
| `chunker.rs` | `chunk_and_store` | FastCDC v2020 chunking of an assembled staging file into content-addressed blobs. |
| `gc.rs` | `CacheGc`, `LeaseSet`, `LeaseGuard` | TTL expiry, `max_bytes` eviction, and an unreferenced-chunk sweep guarded by live-restore read leases. |
| `index.rs` | `CacheIndex`, `IndexEntry`, `IndexRecord` | Persistent, restart-safe cache index: append-only `(scope, version)` JSONL logs mapping client keys to manifest pointers. |
| `manifest.rs` | `ChunkId`, `ChunkRef`, `Manifest` | BLAKE3 chunk ids, chunk refs, and the ranged-read manifest describing an assembled archive. |
| `store.rs` | `CasStore` | The content-addressed store: FastCDC ingest, ranged streamed reads, manifest persistence, and self-healing on a verify-on-read mismatch. |

When you add a file here, add its row above so the index stays current. No
`mod.rs` barrel — declare submodules from the parent file (`src/foo.rs` declares
`mod bar;` for `src/foo/bar.rs`) and import concrete paths.
