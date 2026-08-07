# v1/

**What belongs here:** GHES V1 service-discovery type shapes and pure URL
resolution (`resolve_service_url` and its `timeline_url` / `log_files_url`
callers) — no I/O, matching the crate-wide `protocol` rule.

**What does NOT belong here:** the async HTTP fetch of `/_apis/connectionData`
that produces the `ConnectionData` this module resolves against lives in
`wire::net` (`toolu-runner::net` per this module's doc comment), across the
one-way `protocol` -> `wire` boundary; the V2 (github.com) JIT config and
session types live in the parent crate's `jit_config.rs` / `session.rs`.

## Contents

| File | Primary item | Purpose |
| --- | --- | --- |
| `discovery.rs` | `resolve_service_url`, `timeline_url`, `log_files_url` | Pure lookup of a service URL by matching a service GUID against `ConnectionData`'s service definitions. |
| `types.rs` | `ConnectionData`, `LocationServiceData`, `ServiceDefinition`, `TimelineRecord`, `service_guids`, `api_versions` | GHES V1 protocol wire types (`_apis/connectionData` response shape, timeline record) and the service GUID / API version constants. |

When you add a file here, add its row above so the index stays current. No
`mod.rs` barrel — declare submodules from the parent file (`src/foo.rs` declares
`mod bar;` for `src/foo/bar.rs`) and import concrete paths.
