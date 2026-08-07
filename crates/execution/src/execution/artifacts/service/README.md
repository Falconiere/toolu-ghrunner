# service/

**What belongs here:** the local axum HTTP service that mimics GitHub's
artifact API (`ACTIONS_RUNTIME_URL`) — route handlers and the
start/base_url/shutdown lifecycle over an `ArtifactBackend`.

**What does NOT belong here:** the backend storage implementation itself
(`execution::artifacts::backend::LocalBackend`) — this module only wires HTTP
routes onto it. Bearer validation and generic 401/500 helpers are shared via
`execution::service_auth` / `execution::service_lifecycle`, not redefined
here.

## Contents

| File | Primary item | Purpose |
| --- | --- | --- |
| `handlers.rs` | `handle_create` | HTTP route handlers: create container (POST), upload chunk / finalize (PATCH), list (GET), download by id (GET) — each bearer-checked via `service_auth::validate_bearer`. |
| `lifecycle.rs` | `ArtifactService` | Builds the axum router over the backend, starts it on a random localhost port via `ServiceHandle`, and exposes `base_url()` / `shutdown()`. |

When you add a file here, add its row above so the index stays current. No
`mod.rs` barrel — declare submodules from the parent file (`src/foo.rs`
declares `mod bar;` for `src/foo/bar.rs`) and import concrete paths.
