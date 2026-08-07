# artifacts/

**What belongs here:** the artifact storage abstraction (`ArtifactBackend`)
plus a filesystem-backed implementation, and (in `service/`) the local HTTP
service that mimics GitHub's artifact API for offline mode.

**What does NOT belong here:** the local-vs-real service URL decision
(forwarder / offline / accelerated) — that lives in
`execution::service_endpoints` / `execution::job_runner`, which decide
whether a job even talks to this module's local service or to real GitHub.
Generic axum lifecycle plumbing shared with OIDC/cache lives in
`execution::service_lifecycle`, not duplicated here.

## Contents

| File | Primary item | Purpose |
| --- | --- | --- |
| `backend.rs` | `ArtifactBackend` / `LocalBackend` | The storage trait (create container, upload chunk, finalize, download, list) and its filesystem-backed implementation, with path-traversal validation on every user-supplied component. |
| `service.rs` | (mod decl) | Declares the `service` sub-module and re-exports `ArtifactService`. |

## Sub-modules

- `service/` — the local HTTP service (axum routes + lifecycle) that mimics
  GitHub's artifact API over an `ArtifactBackend`.

When you add a file here, add its row above so the index stays current. No
`mod.rs` barrel — declare submodules from the parent file (`src/foo.rs`
declares `mod bar;` for `src/foo/bar.rs`) and import concrete paths.
