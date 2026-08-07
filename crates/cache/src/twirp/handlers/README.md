# handlers/

**What belongs here:** the three Twirp `CacheService` RPC handlers, one file
per method, each doing its own bearer check and never returning a 5xx for an
ordinary cache miss.

**What does NOT belong here:** the shared `TwirpState`, wire types, and
bearer/host helpers live one level up in `../` (`twirp.rs`, `auth.rs`,
`types.rs`); chunk storage and indexing live in `../../cas/`.

## Contents

| File | Primary item | Purpose |
| --- | --- | --- |
| `create.rs` | `create_cache_entry` | `CreateCacheEntry`: mints a signed upload URL for a new `(scope, key, version)`, or refuses (protected-scope write, duplicate entry) with the load-bearing `cache write denied:` message prefix. |
| `download.rs` | `get_cache_entry_download_url` | `GetCacheEntryDownloadURL`: resolves `(key, restore_keys, version)` through the read ladder to a signed download URL, or a bare `{"ok":false}` miss. |
| `finalize.rs` | `finalize_cache_entry_upload` | `FinalizeCacheEntryUpload`: chunks the staged bytes into the CAS and indexes the entry; a size mismatch or unknown upload rejects with `ok:false` rather than indexing a lying entry. |

When you add a file here, add its row above so the index stays current. No
`mod.rs` barrel — declare submodules from the parent file (`src/foo.rs` declares
`mod bar;` for `src/foo/bar.rs`) and import concrete paths.
