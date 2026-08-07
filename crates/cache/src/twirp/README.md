# twirp/

**What belongs here:** the GitHub Actions Cache Service v2 Twirp RPCs, served
as JSON at `/twirp/github.actions.results.api.v1.CacheService/<Method>` —
bearer auth, wire types, and the shared `TwirpState`, with the three RPC
handlers themselves under `handlers/`.

**What does NOT belong here:** the actual chunk storage and index live in
`../cas/`; the signed URLs this layer mints resolve to the Azure-Blob endpoint
in `../blob/`; the legacy REST v1 protocol lives in `../v1/`.

## Contents

| File | Primary item | Purpose |
| --- | --- | --- |
| `auth.rs` | `check_bearer`, `host_from` | Constant-time bearer check against the forwarded runtime token, and `Host`-header resolution for building signed URLs. |
| `handlers.rs` | (module declarations) | Declares the three RPC handler submodules under `handlers/`. |
| `types.rs` | `CreateRequest`, `CreateResponse`, `DownloadRequest`, `DownloadResponse`, `FinalizeRequest`, `FinalizeResponse` | Snake_case JSON wire types for the three `CacheService` RPCs (int64 fields as decimal strings). |

When you add a file here, add its row above so the index stays current. No
`mod.rs` barrel — declare submodules from the parent file (`src/foo.rs` declares
`mod bar;` for `src/foo/bar.rs`) and import concrete paths.
