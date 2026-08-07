# net/

**What belongs here:** every async HTTP I/O call the runner makes to
GitHub's GitHub Actions JIT protocol — auth token exchange, device-flow
login, App-manifest onboarding, broker session lifecycle, long-poll
message acquire/acknowledge, run-service acquire/renew/complete, results
Twirp RPCs, log blob upload, and GHES V1 service discovery.

**What does NOT belong here:** request/response type definitions and pure
URL/JWT/crypto builders live in `protocol` (sync, no I/O, no network) —
this module is the one-way async transport layer on top of them: every
`pub async fn` here takes a `reqwest::Client` plus a `protocol` request
type and returns a `protocol` response type or `shared::RunnerError`.
Higher-level domain wrappers and reporting-only types live in
`crate::reporting`, which composes these thin transport functions.

## Contents

| File | Primary item | Purpose |
| --- | --- | --- |
| `app_manifest.rs` | `CallbackServer` / `convert_manifest_code` | Loopback HTTP server for the GitHub App manifest flow's redirect callback, plus the `app-manifests/{code}/conversions` POST. |
| `auth.rs` | `authenticate` / `exchange_token` | POSTs the signed JWT to swap it for an OAuth2 access token (the JWT itself is built in `protocol::auth`). |
| `device_auth.rs` | `request_device_code` / `poll_for_token` | GitHub OAuth device-authorization flow: request a device/user code, then poll until an access token or terminal failure. |
| `log_upload.rs` | `upload_block_blob` / `upload_log` | Raw Azure Blob Storage PUT requests (BlockBlob and AppendBlob); the mode decision lives in `crate::reporting::log_upload`. |
| `messages.rs` | `poll_message` / `acknowledge_message` | HTTP transport for the broker long-poll loop; response decryption lives in `protocol::messages`. |
| `register.rs` | `register_jit` / `unregister_runner` | POSTs `generate-jitconfig` to mint a JIT registration, and DELETEs a runner registration. |
| `results_service.rs` | `update_workflow_steps` / signed-blob-URL + metadata calls | Twirp JSON-over-HTTP POSTs to the GitHub Actions Results Service. |
| `run_service.rs` | `acquire_job` / `renew_job` / `complete_job` | HTTP transport for the Actions Run Service job lifecycle; request/response shapes live in `crate::reporting::run_service`. |
| `session.rs` | `create_session` / `delete_session` | HTTP transport for creating and deleting the broker session. |
| `v1.rs` | `fetch_connection_data` / `fetch_timeline` / `post_timeline_record` | HTTP fetches for GHES V1 service discovery and timeline reporting; pure URL resolvers live in `protocol::v1`. |

When you add a file here, add its row above so the index stays current. No
`mod.rs` barrel — declare submodules from the parent file (`src/foo.rs`
declares `mod bar;` for `src/foo/bar.rs`) and import concrete paths.
