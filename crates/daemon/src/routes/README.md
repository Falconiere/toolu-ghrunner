# routes/

**What belongs here:** the daemon's HTTP surface for `vps_hosts` —
wire types, the narrow backend port, shared router state, the bearer-auth
middleware, the `sh-toolu-daemon: 1` header, the error body shape, and
the three route handlers (`POST /v1/jobs`, `DELETE /v1/jobs/{containerId}`,
`DELETE /v1/jobs?jobId=…`). Field names, status codes and the error body
are pinned to `packages/api/src/providers/vps/client.ts` (toolu.sh repo)
byte-for-byte — they are not this crate's to invent.

**What does NOT belong here:** container orchestration. [`backend`]
defines the port the handlers call through, so this module depends only
on "something that can create/destroy/reap a job container" — never on
bollard directly. `crate::docker::DockerBackend` implements that port in
production; `crates/daemon/src/tests/` wires an in-process recorder. The
resource gate's admit/release accounting lives in `crate::gate`, called
directly from `handlers.rs`, not wrapped by this module.

## Contents

| File | Primary item | Purpose |
| --- | --- | --- |
| `wire.rs` | `CreateJobRequest`, `CreateJobResponse`, `ReapQuery` | The exact JSON shapes `client.ts` sends and reads, `camelCase` on the wire via `serde(rename_all)`. |
| `backend.rs` | `JobBackend`, `CreateJobResult`, `CreateError`, `DestroyOutcome`, `ReapOutcome` | The seam between HTTP and container orchestration: create, destroy-by-container-id, reap-by-job-id — deliberately narrow. A reap reports whether it actually settled: the 204 is owed either way, but the job's budget is only released when its containers are provably gone. |
| `state.rs` | `AppState` | Router state threaded through every route: the job backend, the resource gate, the reaper's start queue and created-container map, the token file path, and the one image this host serves (`TOOLU_DAEMON_IMAGE`). |
| `auth_middleware.rs` | `require_bearer` | Verifies `Authorization: Bearer <token>` against the token file (`crate::auth::verify_bearer`) ahead of every route. |
| `header.rs` | `add_daemon_header` | Stamps `sh-toolu-daemon: 1` on every response, including errors, so a Cloudflare-generated 429 or challenge page can be told from a daemon one. |
| `error.rs` | `ErrorBody`, `error_response` | The `{ "error": "<message>" }` shape every non-2xx response goes through, so no handler can hand-roll a different one. |
| `handlers.rs` | `create_job`, `destroy_job`, `reap_job` | The three route handlers `build_router` wires up. A create's bookkeeping runs on a detached task, because the client aborts at ten seconds and axum drops a handler future when it does. |

When you add a file here, add its row above so the index stays current.
There is no `mod.rs`; the parent `routes.rs` is the module root and
declares submodules (`src/foo.rs` declares `mod bar;` for
`src/foo/bar.rs`). Import concrete paths.
