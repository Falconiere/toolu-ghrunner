# v1/

**What belongs here:** the legacy GitHub Actions Cache v1 REST protocol
(`/_apis/artifactcache/*`) re-hosted on the CAS — lookup, reserve, chunked
upload, finalize, and streamed download handlers that speak the exact wire
shapes `actions/cache@v1`–`v4.1` expect.

**What does NOT belong here:** the storage these handlers write into is the
same CAS store and index as v2 — that logic lives in `../cas/`, not here;
the current Twirp v2 protocol lives in `../twirp/`, and the endpoint that
serves the actual bytes for a signed URL lives in `../blob/`.

## Contents

| File | Primary item | Purpose |
| --- | --- | --- |
| `handlers.rs` | `LookupParams`, the five route handlers | The five v1 REST handlers (`cache` lookup, `caches` reserve, chunk `PATCH`, `finalize`, `download`) backed by the CAS store and index; every route but `download` is bearer-checked in constant time. |

When you add a file here, add its row above so the index stays current. No
`mod.rs` barrel — declare submodules from the parent file (`src/foo.rs` declares
`mod bar;` for `src/foo/bar.rs`) and import concrete paths.
