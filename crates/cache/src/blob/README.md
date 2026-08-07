# blob/

**What belongs here:** the Azure-Blob-compatible upload/download endpoint
(`/_toolu/blob/{token}`) that GitHub's `actions/cache` and BuildKit clients
speak — chunked/single-shot uploads, ranged downloads, and the opaque token
registry that ties a URL to a CAS target.

**What does NOT belong here:** the content-addressed chunking and storage
itself lives in `../cas/` (this module only stages bytes and then hands them
to the CAS); the Twirp `CacheService` RPCs that mint the signed URLs pointing
here live in `../twirp/`; the legacy REST v1 protocol lives in `../v1/`.

## Contents

| File | Primary item | Purpose |
| --- | --- | --- |
| `block_list.rs` | `commit` | Azure "Put Block List": assembles committed blocks into the staging file in commit order, then drops the per-upload blocks directory. |
| `get.rs` | `head`, `get` | Download side: `HEAD` returns size via `Content-Length`; `GET` streams the whole object or a byte range, BLAKE3-verifying each chunk under a GC lease. |
| `put_blob.rs` | `put` | Azure "Put Blob" single-shot upload: writes the whole request body verbatim to the staging file. |
| `put_block.rs` | `blocks_dir`, `block_filename` | Azure "Put Block": stages one out-of-order block of a multi-block upload into a per-upload blocks directory. |
| `token.rs` | `BlobRegistry`, `BlobTarget` | In-memory registry of opaque 256-bit, TTL-bound tokens mapping to an upload staging target or a download manifest. |

When you add a file here, add its row above so the index stays current. No
`mod.rs` barrel — declare submodules from the parent file (`src/foo.rs` declares
`mod bar;` for `src/foo/bar.rs`) and import concrete paths.
